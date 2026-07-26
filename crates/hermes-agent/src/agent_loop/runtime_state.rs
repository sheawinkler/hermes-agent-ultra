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

/// Closed, render-only stream observation. Tool inputs and provider metadata
/// never cross this boundary. Stateful controls are repeated on text events so
/// a dropped control-only update cannot leave a renderer permanently stale.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRenderDelta {
    pub text: Option<String>,
    pub mute_post_response: Option<bool>,
    pub break_before: bool,
}

/// Typed, non-blocking observations emitted by a governed agent run.
#[derive(Debug, Clone)]
pub enum AgentObserverEvent {
    Thinking(String),
    ToolStarted { name: String, args_preview: String },
    ToolCompleted { name: String, result_preview: String },
    RenderDelta(AgentRenderDelta),
    Usage(UsageStats),
    StepCompleted(u32),
    Status { kind: String, message: String },
}

#[derive(Debug)]
enum AgentObserverMessage {
    Event(AgentObserverEvent),
    Barrier(tokio::sync::oneshot::Sender<()>),
}

/// Closed channel boundary for progress observers.
///
/// Event delivery is non-blocking. `flush` is an explicit post-run barrier:
/// callers use it only after governed work has stopped emitting observations.
#[derive(Debug, Clone)]
pub struct AgentObserverSink {
    sender: tokio::sync::mpsc::Sender<AgentObserverMessage>,
    dropped_events: Arc<std::sync::atomic::AtomicU64>,
    render_muted: Arc<AtomicBool>,
    render_break_pending: Arc<AtomicBool>,
}

pub struct AgentObserverReceiver {
    receiver: tokio::sync::mpsc::Receiver<AgentObserverMessage>,
}

impl AgentObserverReceiver {
    pub async fn recv(&mut self) -> Option<AgentObserverEvent> {
        loop {
            match self.receiver.recv().await? {
                AgentObserverMessage::Event(event) => return Some(event),
                AgentObserverMessage::Barrier(ack) => {
                    let _ = ack.send(());
                }
            }
        }
    }

    pub fn try_recv(&mut self) -> Option<AgentObserverEvent> {
        loop {
            match self.receiver.try_recv() {
                Ok(AgentObserverMessage::Event(event)) => return Some(event),
                Ok(AgentObserverMessage::Barrier(ack)) => {
                    let _ = ack.send(());
                }
                Err(_) => return None,
            }
        }
    }

    pub fn blocking_recv(&mut self) -> Option<AgentObserverEvent> {
        loop {
            match self.receiver.blocking_recv()? {
                AgentObserverMessage::Event(event) => return Some(event),
                AgentObserverMessage::Barrier(ack) => {
                    let _ = ack.send(());
                }
            }
        }
    }
}

impl AgentObserverSink {
    pub fn channel(capacity: usize) -> (Self, AgentObserverReceiver) {
        let (sender, receiver) = tokio::sync::mpsc::channel(capacity.max(1));
        (
            Self {
                sender,
                dropped_events: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                render_muted: Arc::new(AtomicBool::new(false)),
                render_break_pending: Arc::new(AtomicBool::new(false)),
            },
            AgentObserverReceiver { receiver },
        )
    }

    fn emit(&self, event: AgentObserverEvent) -> bool {
        if self
            .sender
            .try_send(AgentObserverMessage::Event(event))
            .is_err()
        {
            self.dropped_events.fetch_add(1, Ordering::Relaxed);
            false
        } else {
            true
        }
    }

    fn emit_render_text_with_state(
        &self,
        text: &str,
        mute_post_response: Option<bool>,
        break_before: bool,
    ) -> bool {
        const MAX_RENDER_DELTA_BYTES: usize = 4 * 1024;
        if text.is_empty() {
            return false;
        }
        let mut pending_break = break_before;
        let mut break_delivered = false;
        let mut chunk = String::with_capacity(MAX_RENDER_DELTA_BYTES);
        for character in text.chars() {
            if !chunk.is_empty()
                && chunk.len().saturating_add(character.len_utf8()) > MAX_RENDER_DELTA_BYTES
            {
                let accepted = self.emit(AgentObserverEvent::RenderDelta(AgentRenderDelta {
                    text: Some(std::mem::take(&mut chunk)),
                    mute_post_response,
                    break_before: pending_break,
                }));
                if accepted && pending_break {
                    pending_break = false;
                    break_delivered = true;
                }
                chunk = String::with_capacity(MAX_RENDER_DELTA_BYTES);
            }
            chunk.push(character);
        }
        if !chunk.is_empty() {
            let accepted = self.emit(AgentObserverEvent::RenderDelta(AgentRenderDelta {
                text: Some(chunk),
                mute_post_response,
                break_before: pending_break,
            }));
            if accepted && pending_break {
                break_delivered = true;
            }
        }
        break_delivered
    }

    pub(crate) fn emit_render_text(&self, text: &str) {
        let _ = self.emit_render_text_with_state(text, None, false);
    }

    pub(crate) fn emit_stream_chunk(&self, chunk: &StreamChunk) {
        if let Some(usage) = chunk.usage.as_ref() {
            self.emit(AgentObserverEvent::Usage(usage.clone()));
        }
        let Some(delta) = chunk.delta.as_ref() else {
            return;
        };
        let mut control_update = None;
        if let Some(control) = delta
            .extra
            .as_ref()
            .and_then(|extra| extra.get("control"))
            .and_then(Value::as_str)
        {
            match control {
                "mute_post_response" => {
                    let enabled = delta
                        .extra
                        .as_ref()
                        .and_then(|extra| extra.get("enabled"))
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    self.render_muted.store(enabled, Ordering::Release);
                    control_update = Some(enabled);
                }
                "stream_break" => {
                    self.render_break_pending.store(true, Ordering::Release);
                }
                _ => {}
            }
        }
        if let Some(text) = delta.content.as_deref() {
            let muted = self.render_muted.load(Ordering::Acquire);
            if muted {
                if let Some(enabled) = control_update {
                    self.emit(AgentObserverEvent::RenderDelta(AgentRenderDelta {
                        text: None,
                        mute_post_response: Some(enabled),
                        break_before: false,
                    }));
                }
                return;
            }
            let break_before = self.render_break_pending.swap(false, Ordering::AcqRel);
            let break_delivered =
                self.emit_render_text_with_state(text, Some(muted), break_before);
            if break_before && !break_delivered {
                self.render_break_pending.store(true, Ordering::Release);
            }
        } else if let Some(enabled) = control_update {
            self.emit(AgentObserverEvent::RenderDelta(AgentRenderDelta {
                text: None,
                mute_post_response: Some(enabled),
                break_before: false,
            }));
        }
    }

    pub async fn flush(&self) -> bool {
        tokio::time::timeout(Duration::from_secs(2), async {
            let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
            if self
                .sender
                .send(AgentObserverMessage::Barrier(ack_tx))
                .await
                .is_err()
            {
                return false;
            }
            ack_rx.await.is_ok()
        })
        .await
        .unwrap_or(false)
    }

    pub fn dropped_event_count(&self) -> u64 {
        self.dropped_events.load(Ordering::Relaxed)
    }

    pub fn take_dropped_event_count(&self) -> u64 {
        self.dropped_events.swap(0, Ordering::AcqRel)
    }
}

/// Best-effort, data-only observation capability for an agent run.
#[derive(Default)]
pub struct AgentCallbacks {
    pub observer_sink: Option<AgentObserverSink>,
}

impl AgentCallbacks {
    pub(crate) fn emit_thinking(&self, thinking: &str) {
        if let Some(sink) = self.observer_sink.as_ref() {
            sink.emit(AgentObserverEvent::Thinking(
                crate::sealed_artifact::credential_free_observer_text_preview(thinking, 1_024),
            ));
        }
    }

    pub(crate) fn emit_tool_start(&self, name: &str, args: &Value) {
        if let Some(sink) = self.observer_sink.as_ref() {
            sink.emit(AgentObserverEvent::ToolStarted {
                name: name.to_string(),
                args_preview: crate::sealed_artifact::credential_free_observer_preview(args, 512),
            });
        }
    }

    pub(crate) fn emit_tool_complete(&self, name: &str, result: &str) {
        if let Some(sink) = self.observer_sink.as_ref() {
            sink.emit(AgentObserverEvent::ToolCompleted {
                name: name.to_string(),
                result_preview:
                    crate::sealed_artifact::credential_free_observer_text_preview(result, 1_024),
            });
        }
    }

    pub(crate) fn emit_stream_delta(&self, delta: &str) {
        if let Some(sink) = self.observer_sink.as_ref() {
            sink.emit_render_text(delta);
        }
    }

    pub(crate) fn emit_step_complete(&self, turn: u32) {
        if let Some(sink) = self.observer_sink.as_ref() {
            sink.emit(AgentObserverEvent::StepCompleted(turn));
        }
    }

    pub(crate) fn emit_status(&self, kind: &str, message: &str) {
        if let Some(sink) = self.observer_sink.as_ref() {
            sink.emit(AgentObserverEvent::Status {
                kind: crate::sealed_artifact::credential_free_observer_text_preview(kind, 64),
                message: crate::sealed_artifact::credential_free_observer_text_preview(
                    message, 1_024,
                ),
            });
        }
    }
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
          - Re-authenticate: hermes auth add nous\n\
           - Check credits / billing: https://portal.nousresearch.com\n\
           - Verify stored credentials: {}\n\
           - Switch providers temporarily: /model <model> --provider openrouter",
        auth_json.display()
    ))
}

fn classify_error(err: &str) -> ErrorClass {
    let lower = err.to_lowercase();
    if lower.contains("run-record evidence failure") {
        return ErrorClass::Fatal;
    }
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

/// Whether a context error is specifically an oversized output cap rather
/// than an oversized input prompt. Output-cap failures can be retried safely
/// with a smaller `max_tokens`; compaction cannot fix them.
fn is_output_cap_error(err: &str) -> bool {
    let lower = err.to_ascii_lowercase();
    let mentions_output_parameter = lower.contains("max_tokens")
        || lower.contains("max_output_tokens")
        || lower.contains("max_completion_tokens");
    if !mentions_output_parameter {
        return false;
    }

    let output_cap_signal = lower.contains("range of max_tokens should be")
        || lower.contains("available_tokens")
        || lower.contains("available tokens")
        || (lower.contains("in the output") && lower.contains("maximum context length"))
        || (lower.contains("requested") && lower.contains("output tokens"))
        || lower.contains("less than or equal")
        || lower.contains("max_tokens should be");
    if !output_cap_signal {
        return false;
    }

    ![
        "prompt is too long",
        "prompt too long",
        "input is too long",
        "input token",
        "prompt length",
        "prompt contains",
        "reduce the length",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

/// Extract a provider-reported output budget from an output-cap error.
///
/// This intentionally handles the several observed provider shapes rather
/// than treating every context error as an input-overflow/compaction problem.
fn output_cap_available_tokens(err: &str) -> Option<u64> {
    if !is_output_cap_error(err) {
        return None;
    }
    let lower = err.to_ascii_lowercase();

    let range = regex::Regex::new(
        r"range of max_tokens should be\s*\[\s*\d+\s*,\s*(\d+)\s*\]",
    )
    .expect("static output-cap range regex must compile");
    if let Some(tokens) = range
        .captures(&lower)
        .and_then(|captures| captures.get(1))
        .and_then(|capture| capture.as_str().parse::<u64>().ok())
        .filter(|tokens| *tokens >= 1)
    {
        return Some(tokens);
    }

    for pattern in [
        r"available_tokens[:\s]+(\d+)",
        r"available\s+tokens[:\s]+(\d+)",
        r"=\s*(\d+)\s*$",
    ] {
        let re = regex::Regex::new(pattern).expect("static output-cap regex must compile");
        if let Some(tokens) = re
            .captures(&lower)
            .and_then(|captures| captures.get(1))
            .and_then(|capture| capture.as_str().parse::<u64>().ok())
            .filter(|tokens| *tokens >= 1)
        {
            return Some(tokens);
        }
    }

    let context = regex::Regex::new(r"maximum context length is\s+(\d+)")
        .expect("static context-limit regex must compile");
    let parts = regex::Regex::new(
        r"\((\d+)\s+of text input,\s*(\d+)\s+of tool input,\s*(\d+)\s+in the output\)",
    )
    .expect("static context-part regex must compile");
    let context_tokens = context
        .captures(&lower)
        .and_then(|captures| captures.get(1))
        .and_then(|capture| capture.as_str().parse::<u64>().ok())?;
    let parts = parts.captures(&lower)?;
    let text_tokens = parts.get(1)?.as_str().parse::<u64>().ok()?;
    let tool_tokens = parts.get(2)?.as_str().parse::<u64>().ok()?;
    context_tokens
        .checked_sub(text_tokens.saturating_add(tool_tokens))
        .filter(|tokens| *tokens >= 1)
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
        | AgentError::PolicyBlocked(_)
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

#[derive(Debug, Clone, Default, Serialize)]
struct RouteLearningStats {
    samples: u32,
    success_rate: f64,
    avg_latency_ms: f64,
    consecutive_failures: u32,
    updated_at_unix_ms: i64,
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

#[cfg(test)]
mod observer_tests {
    use super::*;

    fn stream_chunk(content: Option<&str>, extra: Option<Value>) -> StreamChunk {
        StreamChunk {
            delta: Some(hermes_core::StreamDelta {
                content: content.map(str::to_string),
                tool_calls: None,
                extra,
            }),
            finish_reason: None,
            usage: None,
        }
    }

    #[tokio::test]
    async fn bounded_observer_counts_dropped_events() {
        let (sink, mut receiver) = AgentObserverSink::channel(1);
        let callbacks = AgentCallbacks {
            observer_sink: Some(sink.clone()),
        };
        callbacks.emit_thinking("first");
        callbacks.emit_status("busy", "second");

        assert_eq!(sink.dropped_event_count(), 1);
        assert!(matches!(
            receiver.recv().await,
            Some(AgentObserverEvent::Thinking(text)) if text == "first"
        ));
        assert_eq!(sink.take_dropped_event_count(), 1);
        assert_eq!(sink.dropped_event_count(), 0);
    }

    #[tokio::test]
    async fn renderer_boundary_suppresses_muted_text_and_preserves_break() {
        let (sink, mut receiver) = AgentObserverSink::channel(8);
        sink.emit_stream_chunk(&stream_chunk(
            None,
            Some(serde_json::json!({
                "control": "mute_post_response",
                "enabled": true
            })),
        ));
        sink.emit_stream_chunk(&stream_chunk(Some("secret post-response text"), None));
        sink.emit_stream_chunk(&stream_chunk(
            None,
            Some(serde_json::json!({
                "control": "mute_post_response",
                "enabled": false
            })),
        ));
        sink.emit_stream_chunk(&stream_chunk(
            None,
            Some(serde_json::json!({"control": "stream_break"})),
        ));
        sink.emit_stream_chunk(&stream_chunk(Some("visible"), None));

        assert!(matches!(
            receiver.recv().await,
            Some(AgentObserverEvent::RenderDelta(AgentRenderDelta {
                text: None,
                mute_post_response: Some(true),
                break_before: false,
            }))
        ));
        assert!(matches!(
            receiver.recv().await,
            Some(AgentObserverEvent::RenderDelta(AgentRenderDelta {
                text: None,
                mute_post_response: Some(false),
                break_before: false,
            }))
        ));
        assert!(matches!(
            receiver.recv().await,
            Some(AgentObserverEvent::RenderDelta(AgentRenderDelta {
                text: Some(text),
                mute_post_response: Some(false),
                break_before: true,
            })) if text == "visible"
        ));
        assert!(receiver.try_recv().is_none());
    }

    #[tokio::test]
    async fn flush_waits_until_prior_observations_are_received() {
        let (sink, mut receiver) = AgentObserverSink::channel(4);
        let callbacks = AgentCallbacks {
            observer_sink: Some(sink.clone()),
        };
        callbacks.emit_step_complete(3);
        let drain = tokio::spawn(async move {
            let mut events = Vec::new();
            while let Some(event) = receiver.recv().await {
                events.push(event);
            }
            events
        });

        assert!(sink.flush().await);
        drop(callbacks);
        drop(sink);
        let events = drain.await.expect("observer drain task");
        assert_eq!(events.len(), 1);
        assert!(matches!(
            events.first(),
            Some(AgentObserverEvent::StepCompleted(3))
        ));
    }
}
