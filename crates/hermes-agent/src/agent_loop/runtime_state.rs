// ---------------------------------------------------------------------------
// TurnMetrics
// ---------------------------------------------------------------------------

/// Timing and usage metrics for a single agent turn.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TurnMetrics {
    /// Wall-clock time spent waiting for the LLM API, in milliseconds.
    pub api_time_ms: u64,
    /// Wall-clock time spent executing tools, in milliseconds.
    pub tool_time_ms: u64,
    /// Token usage for this turn (if reported by the provider).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<UsageStats>,
}

// ---------------------------------------------------------------------------
// Evolution counters (Python `_turns_since_memory` / `_iters_since_skill`)
// ---------------------------------------------------------------------------

/// Session-scoped counters for memory / skill nudges (mirrors Python `AIAgent` fields).
#[derive(Debug, Default)]
pub struct EvolutionCounters {
    pub turns_since_memory: u32,
    pub iters_since_skill: u32,
}

// ---------------------------------------------------------------------------
// AgentLoop
// ---------------------------------------------------------------------------

/// Callbacks invoked during tool execution for progress reporting.
#[derive(Default)]
pub struct AgentCallbacks {
    /// Called when the LLM is "thinking" (reasoning tokens).
    pub on_thinking: Option<Box<dyn Fn(&str) + Send + Sync>>,
    /// Called when a tool call begins.
    pub on_tool_start: Option<Box<dyn Fn(&str, &Value) + Send + Sync>>,
    /// Called when a tool call finishes.
    pub on_tool_complete: Option<Box<dyn Fn(&str, &str) + Send + Sync>>,
    /// Called for each stream delta.
    pub on_stream_delta: Option<Box<dyn Fn(&str) + Send + Sync>>,
    /// Called after each completed LLM step (full response assembled).
    pub on_step_complete: Option<Box<dyn Fn(u32) + Send + Sync>>,
    /// Called when background memory/skill review completes or fails.
    ///
    /// Payload is a user-friendly summary string suitable for direct UI output.
    pub background_review_callback: Option<Arc<dyn Fn(&str) + Send + Sync>>,
    /// Called when `delegate_task(background=true)` completes out-of-band.
    ///
    /// Payload is a self-contained summary suitable for reinjection into the
    /// originating UI or platform conversation.
    pub background_delegation_callback: Option<Arc<dyn Fn(&str) + Send + Sync>>,
    /// Called for lifecycle/status notices (context pressure, retries, etc.).
    pub status_callback: Option<Arc<dyn Fn(&str, &str) + Send + Sync>>,
}

/// Classify an API error for retry/failover decisions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorClass {
    Retryable,
    RateLimit,
    ContextOverflow,
    Auth,
    Fatal,
}

fn has_ssl_transient_phrase(lower: &str) -> bool {
    lower.contains("bad record mac")
        || lower.contains("ssl alert")
        || lower.contains("tls alert")
        || lower.contains("ssl handshake failure")
        || lower.contains("tlsv1 alert")
        || lower.contains("sslv3 alert")
        || lower.contains("bad_record_mac")
        || lower.contains("ssl_alert")
        || lower.contains("tls_alert")
        || lower.contains("tls_alert_internal_error")
        || lower.contains("[ssl:")
}

fn maybe_nous_401_diagnostic(
    provider_hint: &str,
    err: &str,
    hermes_home: Option<&str>,
) -> Option<String> {
    let provider = provider_hint.trim().to_ascii_lowercase();
    if !provider.starts_with("nous") {
        return None;
    }
    let lower = err.to_ascii_lowercase();
    let is_auth_401 =
        lower.contains("401") || lower.contains("unauthorized") || lower.contains("authentication");
    if !is_auth_401 {
        return None;
    }

    let response = err.replace('\n', " ");
    let response_snippet = if response.len() > 200 {
        format!("{}...", &response[..200])
    } else {
        response
    };
    let auth_json = hermes_home
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".hermes-agent-ultra")
        })
        .join("auth.json");

    Some(format!(
        "Nous 401 — Portal authentication failed.\n\
         Response: {response_snippet}\n\
         Most likely: Portal OAuth expired, account out of credits, or agent key revoked.\n\
         Troubleshooting:\n\
           - Re-authenticate: hermes auth login nous\n\
           - Check credits / billing: https://portal.nousresearch.com\n\
           - Verify stored credentials: {}\n\
           - Switch providers temporarily: /model <model> --provider openrouter",
        auth_json.display()
    ))
}

fn classify_error(err: &str) -> ErrorClass {
    let lower = err.to_lowercase();
    let model_not_found = lower.contains("model not found")
        || lower.contains("invalid model")
        || lower.contains("no such model")
        || lower.contains("unknown model");
    let openrouter_privacy_guardrail =
        lower.contains("privacy guardrail") || lower.contains("openrouter privacy");

    if lower.contains("rate limit")
        || lower.contains("429")
        || lower.contains("too many")
        || lower.contains("throttlingexception")
    {
        ErrorClass::RateLimit
    } else if lower.contains("404") || lower.contains("not found") {
        if model_not_found || openrouter_privacy_guardrail {
            ErrorClass::Fatal
        } else {
            ErrorClass::Retryable
        }
    } else if lower.contains("413")
        || lower.contains("payload too large")
        || lower.contains("request too large")
        || lower.contains("context length")
        || lower.contains("maximum context")
        || lower.contains("context window")
        || lower.contains("token limit")
        || lower.contains("context_length_exceeded")
        || lower.contains("input is too long")
        || lower.contains("prompt is too long")
        || lower.contains("reduce the length")
    {
        ErrorClass::ContextOverflow
    } else if lower.contains("401")
        || lower.contains("403")
        || lower.contains("unauthorized")
        || lower.contains("authentication")
    {
        ErrorClass::Auth
    } else if has_ssl_transient_phrase(&lower) {
        ErrorClass::Retryable
    } else if lower.contains("500")
        || lower.contains("502")
        || lower.contains("503")
        || lower.contains("timeout")
        || lower.contains("connection")
        || lower.contains("disconnected")
        || lower.contains("overloaded")
    {
        ErrorClass::Retryable
    } else {
        ErrorClass::Fatal
    }
}

fn provider_or_base_url_uses_cloudcode_quota(provider: &str, base_url: Option<&str>) -> bool {
    let provider = provider.trim().to_ascii_lowercase();
    matches!(
        provider.as_str(),
        "google-gemini-cli" | "gemini-cli" | "gemini-oauth"
    ) || base_url
        .map(str::trim)
        .map(|url| url.to_ascii_lowercase().starts_with("cloudcode-pa://"))
        .unwrap_or(false)
}

fn credential_pool_may_recover_from_rate_limit(
    pool: Option<&Arc<CredentialPool>>,
    provider: &str,
    base_url: Option<&str>,
) -> bool {
    let Some(pool) = pool else {
        return false;
    };
    if provider_or_base_url_uses_cloudcode_quota(provider, base_url) {
        return false;
    }
    pool.has_available() && pool.len() > 1
}

fn is_tool_payload_validation_error(err: &str) -> bool {
    let lower = err.to_ascii_lowercase();
    (lower.contains("invalid input") && lower.contains("function"))
        || lower.contains("provider returned error")
            && (lower.contains("request is not valid") || lower.contains("check the model name"))
        || (lower.contains("no choices in response") || lower.contains("empty choices array"))
            && (lower.contains("request is not valid")
                || lower.contains("valid payload")
                || lower.contains("provider returned error")
                || lower.contains("tool"))
        || lower.contains("unprocessable entity") && lower.contains("valid payload")
        || lower.contains("tools") && lower.contains("invalid")
}

fn preferred_tool_payload_fallback_model(provider_hint: &str, model_name: &str) -> Option<String> {
    let provider = provider_hint.trim().to_ascii_lowercase();
    let model = model_name.trim().to_ascii_lowercase();
    let nous_openai_route = matches!(
        provider.as_str(),
        "nous" | "nous-api" | "nous_api" | "nousapi" | "nous-portal-api"
    ) && model.starts_with("openai/");
    if !nous_openai_route {
        return None;
    }
    if let Ok(override_model) = std::env::var("HERMES_TOOL_PAYLOAD_FALLBACK_MODEL") {
        let trimmed = override_model.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    Some("nousresearch/hermes-4-70b".to_string())
}

fn is_transient_stream_error(err: &AgentError) -> bool {
    fn has_transient_phrase(msg: &str) -> bool {
        let lower = msg.to_lowercase();
        lower.contains("timeout")
            || lower.contains("connection")
            || lower.contains("disconnected")
            || lower.contains("remoteprotocol")
            || lower.contains("remote protocol")
            || lower.contains("network error")
            || lower.contains("broken pipe")
            || lower.contains("connection reset")
            || lower.contains("connection closed")
            || lower.contains("connection lost")
            || lower.contains("upstream connect error")
            || lower.contains("stream read error")
            || has_ssl_transient_phrase(&lower)
    }

    match err {
        AgentError::Timeout(_) => true,
        AgentError::LlmApi(msg)
        | AgentError::Gateway(msg)
        | AgentError::Io(msg)
        | AgentError::ToolExecution(msg)
        | AgentError::Config(msg)
        | AgentError::AuthFailed(msg)
        | AgentError::InvalidToolCall(msg) => has_transient_phrase(msg),
        AgentError::RateLimited { .. } => true,
        AgentError::Interrupted { .. }
        | AgentError::MaxTurnsExceeded
        | AgentError::ContextTooLong => false,
    }
}

/// Compute jittered exponential backoff delay.
fn jittered_backoff(attempt: u32, base_ms: u64, max_ms: u64) -> Duration {
    let exp = base_ms.saturating_mul(1u64 << attempt.min(10));
    let capped = exp.min(max_ms);
    let jitter = capped / 4;
    let delay = capped.saturating_sub(jitter / 2) + (rand_u64_range(0, jitter.max(1)));
    Duration::from_millis(delay)
}

fn rand_u64_range(min: u64, max: u64) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    std::time::SystemTime::now().hash(&mut hasher);
    std::thread::current().id().hash(&mut hasher);
    let h = hasher.finish();
    if max <= min {
        min
    } else {
        min + h % (max - min)
    }
}

fn advertised_tool_name_index(tool_schemas: &[ToolSchema]) -> (HashSet<&str>, String) {
    let mut names: Vec<&str> = tool_schemas
        .iter()
        .map(|schema| schema.name.as_str())
        .collect();
    names.sort_unstable();
    names.dedup();
    let display = names.join(", ");
    (names.into_iter().collect(), display)
}

/// Result of collecting one streaming completion (may end with user interrupt).
enum StreamCollectOutcome {
    Complete(LlmResponse),
    Interrupted(LlmResponse),
}

#[derive(Debug, Clone, Copy)]
struct TurnGovernor {
    max_tokens: Option<u32>,
    tool_concurrency: usize,
    pressure: f64,
    latency_degraded: bool,
    error_degraded: bool,
}

#[derive(Debug, Clone, Copy, Default)]
struct GovernorRuntimeState {
    avg_llm_latency_ms: Option<f64>,
    avg_tool_error_rate: f64,
    consecutive_error_turns: u32,
}

#[derive(Debug, Clone, Default)]
struct RepoReviewBudgetState {
    last_discovery_signature: Option<String>,
    repeat_streak: u32,
    low_signal_streak: u32,
    last_signal_score: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct RouteLearningStats {
    samples: u32,
    success_rate: f64,
    avg_latency_ms: f64,
    consecutive_failures: u32,
    updated_at_unix_ms: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct RouteLearningState {
    schema_version: u32,
    saved_at_unix_ms: i64,
    entries: HashMap<String, RouteLearningStats>,
}

#[derive(Debug, Clone)]
struct ReplayRecorder {
    path: Option<PathBuf>,
    state: Option<Arc<Mutex<ReplayState>>>,
}

#[derive(Debug, Clone)]
struct ReplayState {
    seq: u64,
    prev_hash: String,
    trace_root: String,
}

impl ReplayRecorder {
    fn for_session(config: &AgentConfig, session_id: &str) -> Self {
        let enabled = std::env::var("HERMES_REPLAY_ENABLED")
            .map(|v| {
                matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            })
            .unwrap_or(false);
        if !enabled {
            return Self {
                path: None,
                state: None,
            };
        }
        let root = config
            .hermes_home
            .as_deref()
            .map(PathBuf::from)
            .or_else(|| std::env::var("HERMES_HOME").ok().map(PathBuf::from))
            .or_else(|| {
                std::env::var("HOME")
                    .ok()
                    .map(|home| PathBuf::from(home).join(".hermes-agent-ultra"))
            })
            .unwrap_or_else(|| PathBuf::from(".hermes-agent-ultra"));
        let dir = root.join("logs").join("replay");
        if std::fs::create_dir_all(&dir).is_err() {
            return Self {
                path: None,
                state: None,
            };
        }
        let sid = if session_id.trim().is_empty() {
            format!("session-{}", chrono::Utc::now().format("%Y%m%dT%H%M%SZ"))
        } else {
            session_id
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                        c
                    } else {
                        '_'
                    }
                })
                .collect::<String>()
        };
        let initial_prev_hash = short_sha256_hex(&format!("session:{sid}:v1"));
        let trace_root = short_sha256_hex(&format!("trace:{sid}:v1"));
        Self {
            path: Some(dir.join(format!("{sid}.jsonl"))),
            state: Some(Arc::new(Mutex::new(ReplayState {
                seq: 0,
                prev_hash: initial_prev_hash,
                trace_root,
            }))),
        }
    }

    fn record(&self, event: &str, payload: Value) {
        let Some(path) = self.path.as_ref() else {
            return;
        };
        let Some(state) = self.state.as_ref() else {
            return;
        };
        let mut redacted = payload;
        redact_json_value(&mut redacted);
        let canonical_payload =
            serde_json::to_string(&redacted).unwrap_or_else(|_| "{}".to_string());
        let (seq, prev_hash, event_hash, trace_id) = {
            let mut guard = state.lock().unwrap();
            guard.seq = guard.seq.saturating_add(1);
            let seq = guard.seq;
            let prev_hash = guard.prev_hash.clone();
            let event_hash =
                short_sha256_hex(&format!("{seq}|{event}|{prev_hash}|{canonical_payload}"));
            let trace_id = format!("{}-{:08x}", guard.trace_root, seq);
            guard.prev_hash = event_hash.clone();
            (seq, prev_hash, event_hash, trace_id)
        };
        let line = serde_json::json!({
            "ts": chrono::Utc::now().to_rfc3339(),
            "seq": seq,
            "trace_id": trace_id,
            "event": event,
            "prev_hash": prev_hash,
            "event_hash": event_hash,
            "payload": redacted,
        })
        .to_string();
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(f, "{line}");
        }
    }
}

fn replay_sensitive_key(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    k.contains("api_key")
        || k.contains("token")
        || k.contains("secret")
        || k.contains("password")
        || k.contains("authorization")
        || k.contains("cookie")
        || k.contains("session")
}

fn short_sha256_hex(input: &str) -> String {
    let digest = Sha256::digest(input.as_bytes());
    let mut out = String::with_capacity(16);
    for b in digest.iter().take(8) {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{:02x}", b);
    }
    out
}

fn redact_sensitive_text(value: &str) -> Option<String> {
    lazy_static::lazy_static! {
        static ref SECRET_PATTERNS: Vec<regex::Regex> = vec![
            regex::Regex::new(r"(?i)bearer\\s+[A-Za-z0-9._\\-]{8,}").unwrap(),
            regex::Regex::new(r"sk-[A-Za-z0-9]{8,}").unwrap(),
            regex::Regex::new(r"gh[pousr]_[A-Za-z0-9]{12,}").unwrap(),
            regex::Regex::new(r"xox[baprs]-[A-Za-z0-9\\-]{10,}").unwrap(),
            regex::Regex::new(r"(?i)(api[_-]?key|token|secret|password)\\s*[:=]\\s*[A-Za-z0-9._\\-]{6,}").unwrap(),
        ];
    }
    let mut redacted = value.to_string();
    let mut changed = false;
    for pattern in SECRET_PATTERNS.iter() {
        let next = pattern.replace_all(&redacted, "[redacted]").to_string();
        if next != redacted {
            changed = true;
            redacted = next;
        }
    }
    if changed {
        Some(redacted)
    } else {
        None
    }
}

fn truncate_hook_preview(text: &str, max_chars: usize) -> String {
    let total = text.chars().count();
    if total <= max_chars.max(1) {
        return text.to_string();
    }
    let keep_head = max_chars.saturating_sub(96).max(64);
    let head: String = text.chars().take(keep_head).collect();
    let omitted = total.saturating_sub(keep_head);
    format!("{head}\n...[truncated {omitted} chars]...")
}

fn redact_json_value(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (k, v) in map.iter_mut() {
                if replay_sensitive_key(k) {
                    *v = Value::String("[redacted]".to_string());
                } else {
                    redact_json_value(v);
                }
            }
        }
        Value::Array(arr) => {
            for v in arr {
                redact_json_value(v);
            }
        }
        Value::String(raw) => {
            if let Some(redacted) = redact_sensitive_text(raw) {
                *raw = redacted;
            }
        }
        _ => {}
    }
}

fn governor_enabled() -> bool {
    std::env::var("HERMES_PERFORMANCE_GOVERNOR")
        .map(|v| {
            !matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "off"
            )
        })
        .unwrap_or(true)
}

fn governor_tool_concurrency_base() -> usize {
    std::env::var("HERMES_TOOL_CALL_MAX_CONCURRENCY")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(8)
}

fn governor_window_size() -> usize {
    std::env::var("HERMES_PERF_GOV_WINDOW")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(8)
}

fn governor_latency_warn_ms() -> f64 {
    std::env::var("HERMES_PERF_GOV_LATENCY_WARN_MS")
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| *v > 0.0)
        .unwrap_or(3500.0)
}

fn governor_latency_critical_ms() -> f64 {
    std::env::var("HERMES_PERF_GOV_LATENCY_CRITICAL_MS")
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| *v > 0.0)
        .unwrap_or(6500.0)
}

fn governor_error_warn_rate() -> f64 {
    std::env::var("HERMES_PERF_GOV_ERROR_WARN_RATE")
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| (0.0..=1.0).contains(v))
        .unwrap_or(0.20)
}

fn governor_error_critical_rate() -> f64 {
    std::env::var("HERMES_PERF_GOV_ERROR_CRITICAL_RATE")
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| (0.0..=1.0).contains(v))
        .unwrap_or(0.50)
}

fn governor_tool_loop_guard_enabled() -> bool {
    std::env::var("HERMES_TOOL_LOOP_GUARD_ENABLED")
        .map(|v| {
            !matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "off"
            )
        })
        .unwrap_or(true)
}

fn governor_tool_loop_guard_max_consecutive_error_turns() -> u32 {
    std::env::var("HERMES_TOOL_LOOP_GUARD_MAX_CONSEC_ERROR_TURNS")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(4)
}

fn governor_tool_loop_guard_min_failed_calls() -> u32 {
    std::env::var("HERMES_TOOL_LOOP_GUARD_MIN_FAILED_CALLS")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(1)
}

fn should_trip_tool_loop_guard(
    consecutive_error_turns: u32,
    turn_tool_count: usize,
    turn_tool_error_count: u32,
) -> bool {
    should_trip_tool_loop_guard_with_config(
        consecutive_error_turns,
        turn_tool_count,
        turn_tool_error_count,
        governor_tool_loop_guard_enabled(),
        governor_tool_loop_guard_max_consecutive_error_turns(),
        governor_tool_loop_guard_min_failed_calls(),
    )
}

fn should_trip_tool_loop_guard_with_config(
    consecutive_error_turns: u32,
    turn_tool_count: usize,
    turn_tool_error_count: u32,
    enabled: bool,
    max_consecutive_error_turns: u32,
    min_failed_calls: u32,
) -> bool {
    if !enabled {
        return false;
    }
    if turn_tool_count == 0 {
        return false;
    }
    if turn_tool_error_count < min_failed_calls {
        return false;
    }
    if turn_tool_error_count != turn_tool_count as u32 {
        return false;
    }
    consecutive_error_turns >= max_consecutive_error_turns
}

fn looks_like_tool_error_output(output: &str) -> bool {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return false;
    }
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        if let Some(obj) = value.as_object() {
            if let Some(err) = obj.get("error") {
                if !err.is_null() {
                    return true;
                }
            }
            if let Some(success) = obj.get("success").and_then(|v| v.as_bool()) {
                if !success {
                    return true;
                }
            }
            if let Some(status) = obj.get("status").and_then(|v| v.as_str()) {
                if status.eq_ignore_ascii_case("error") || status.eq_ignore_ascii_case("failed") {
                    return true;
                }
            }
        }
    }
    let lower = trimmed.to_ascii_lowercase();
    lower.starts_with("error:")
        || lower.contains("invalid tool parameters")
        || lower.contains("missing '")
}

fn smart_routing_learning_enabled() -> bool {
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

fn smart_routing_learning_alpha() -> f64 {
    std::env::var("HERMES_SMART_ROUTING_LEARNING_ALPHA")
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| (0.01..=1.0).contains(v))
        .unwrap_or(0.20)
}

fn smart_routing_learning_cheap_bias() -> f64 {
    std::env::var("HERMES_SMART_ROUTING_LEARNING_CHEAP_BIAS")
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| (-0.50..=0.50).contains(v))
        .unwrap_or(0.08)
}

fn smart_routing_learning_switch_margin() -> f64 {
    std::env::var("HERMES_SMART_ROUTING_LEARNING_SWITCH_MARGIN")
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| (0.0..=0.50).contains(v))
        .unwrap_or(0.03)
}

fn smart_routing_learning_ttl_secs() -> i64 {
    std::env::var("HERMES_SMART_ROUTING_LEARNING_TTL_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|v| *v >= 0)
        .unwrap_or(7 * 24 * 60 * 60)
}

fn smart_routing_learning_half_life_secs() -> i64 {
    std::env::var("HERMES_SMART_ROUTING_LEARNING_HALF_LIFE_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|v| *v >= 0)
        .unwrap_or(24 * 60 * 60)
}

fn now_unix_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn default_route_learning_home(config: &AgentConfig) -> PathBuf {
    config
        .hermes_home
        .as_deref()
        .map(PathBuf::from)
        .or_else(|| std::env::var("HERMES_HOME").ok().map(PathBuf::from))
        .or_else(|| {
            std::env::var("HERMES_AGENT_ULTRA_HOME")
                .ok()
                .map(PathBuf::from)
        })
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .map(|home| PathBuf::from(home).join(".hermes-agent-ultra"))
        })
        .unwrap_or_else(|| PathBuf::from(".hermes-agent-ultra"))
}

fn route_learning_state_path(config: &AgentConfig) -> PathBuf {
    default_route_learning_home(config)
        .join("logs")
        .join("route-learning.json")
}

fn push_window_u64(window: &mut VecDeque<u64>, value: u64, limit: usize) {
    window.push_back(value);
    while window.len() > limit {
        let _ = window.pop_front();
    }
}

fn push_window_f64(window: &mut VecDeque<f64>, value: f64, limit: usize) {
    window.push_back(value);
    while window.len() > limit {
        let _ = window.pop_front();
    }
}

fn avg_u64(window: &VecDeque<u64>) -> Option<f64> {
    if window.is_empty() {
        return None;
    }
    Some(window.iter().copied().map(|v| v as f64).sum::<f64>() / window.len() as f64)
}

fn avg_f64(window: &VecDeque<f64>) -> f64 {
    if window.is_empty() {
        return 0.0;
    }
    window.iter().copied().sum::<f64>() / window.len() as f64
}

fn governor_runtime_state(
    llm_latency_window: &VecDeque<u64>,
    tool_error_window: &VecDeque<f64>,
    consecutive_error_turns: u32,
) -> GovernorRuntimeState {
    GovernorRuntimeState {
        avg_llm_latency_ms: avg_u64(llm_latency_window),
        avg_tool_error_rate: avg_f64(tool_error_window),
        consecutive_error_turns,
    }
}

fn governor_for_turn(
    config: &AgentConfig,
    ctx: &ContextManager,
    requested_tools: usize,
    runtime: Option<&GovernorRuntimeState>,
) -> TurnGovernor {
    let threshold = ((ctx.max_context_chars().max(1) as f64) * 0.8).max(1.0);
    let mut pressure = (ctx.total_chars() as f64 / threshold).max(0.0);
    let enabled = governor_enabled();
    let mut latency_degraded = false;
    let mut error_degraded = false;

    if enabled {
        if let Some(runtime) = runtime {
            if let Some(lat_ms) = runtime.avg_llm_latency_ms {
                if lat_ms >= governor_latency_critical_ms() {
                    pressure = pressure.max(0.97);
                    latency_degraded = true;
                } else if lat_ms >= governor_latency_warn_ms() {
                    pressure = pressure.max(0.88);
                    latency_degraded = true;
                }
            }
            if runtime.avg_tool_error_rate >= governor_error_critical_rate()
                || runtime.consecutive_error_turns >= 3
            {
                pressure = pressure.max(0.97);
                error_degraded = true;
            } else if runtime.avg_tool_error_rate >= governor_error_warn_rate()
                || runtime.consecutive_error_turns >= 1
            {
                pressure = pressure.max(0.88);
                error_degraded = true;
            }
        }
    }

    let max_tokens = if enabled {
        config.max_tokens.map(|base| {
            if pressure >= 0.95 {
                base.saturating_div(4).max(64)
            } else if pressure >= 0.85 {
                base.saturating_div(2).max(128)
            } else {
                base
            }
        })
    } else {
        config.max_tokens
    };

    let base_concurrency = governor_tool_concurrency_base();
    let mut tool_concurrency = if enabled {
        if pressure >= 0.95 {
            base_concurrency.min(2)
        } else if pressure >= 0.85 {
            base_concurrency.min(4)
        } else {
            base_concurrency
        }
    } else {
        base_concurrency
    };
    if requested_tools > 0 {
        tool_concurrency = tool_concurrency.min(requested_tools).max(1);
    }

    TurnGovernor {
        max_tokens,
        tool_concurrency,
        pressure,
        latency_degraded,
        error_degraded,
    }
}

fn runtime_provider_allows_no_api_key(provider: &str, base_url: Option<&str>) -> bool {
    crate::local_backends::is_local_backend_provider(provider)
        || base_url.is_some_and(crate::local_backends::is_local_or_private_base_url)
}

