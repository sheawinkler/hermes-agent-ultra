//! Telemetry bootstrap and in-process metrics registry.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use tracing_subscriber::fmt::format::{self, FormatEvent, FormatFields};
use tracing_subscriber::fmt::FmtContext;
use tracing_subscriber::prelude::*;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Registry;

/// ANSI color codes for log levels (used only when output is a TTY).
fn level_color(level: tracing::Level) -> (&'static str, &'static str) {
    // (prefix, reset)
    match level {
        tracing::Level::ERROR => ("\x1b[31m", "\x1b[0m"), // red
        tracing::Level::WARN => ("\x1b[33m", "\x1b[0m"),  // yellow
        tracing::Level::INFO => ("\x1b[32m", "\x1b[0m"),  // green
        tracing::Level::DEBUG => ("\x1b[36m", "\x1b[0m"), // cyan
        tracing::Level::TRACE => ("\x1b[35m", "\x1b[0m"), // magenta
    }
}

/// Custom event formatter.
///
/// Output: `{local_rfc3339} {LEVEL:<5} [file:line] thread={name} {message}`
///
/// - Level is ANSI-colored when stderr is a TTY.
/// - `target` is omitted; `file:line` carries enough location context.
struct LocalFormatter;

impl<S, N> FormatEvent<S, N> for LocalFormatter
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: format::Writer<'_>,
        event: &tracing::Event<'_>,
    ) -> std::fmt::Result {
        let ts = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ");
        let meta = event.metadata();
        let level = *meta.level();
        let pid = std::process::id();

        let location = match (meta.file(), meta.line()) {
            (Some(f), Some(l)) => format!("{}:{}", f, l),
            (Some(f), None) => f.to_string(),
            _ => String::new(),
        };

        let thread = std::thread::current();
        let thread_name = thread.name().unwrap_or("?");

        if writer.has_ansi_escapes() {
            let (color, reset) = level_color(level);
            write!(
                writer,
                "{} {color}{:<5}{reset} [{}] pid={} thread={} ",
                ts, level, location, pid, thread_name,
                color = color,
                reset = reset,
            )?;
        } else {
            write!(
                writer,
                "{} {:<5} [{}] pid={} thread={} ",
                ts, level, location, pid, thread_name
            )?;
        }

        ctx.format_fields(writer.by_ref(), event)?;
        writeln!(writer)
    }
}

#[cfg(feature = "otlp")]
mod otlp;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryConfig {
    pub level: String,
    pub json: bool,
    pub service_name: String,
    /// OTLP HTTP traces URL base (`http://host:4318`) or full path (`.../v1/traces`).
    /// Active when crate is built with `--features otlp`.
    pub otlp_endpoint: Option<String>,
}

/// Log file handle cached for the process lifetime.
///
/// Opens once on first write; subsequent calls reuse the same `File`.
/// Wrapped in `Mutex` because `MakeWriter::make_writer` requires `&self` and
/// `tracing-subscriber` may call it from multiple threads concurrently.
static LOG_FILE: OnceLock<Option<Mutex<File>>> = OnceLock::new();

fn open_log_file() -> Option<Mutex<File>> {
    let path = default_log_file_path()?;
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .ok()
        .map(Mutex::new)
}

/// `MakeWriter` that tees every log line to stderr **and** a rotating log file.
///
/// The file handle is opened once (lazy, process-scoped) via [`LOG_FILE`].
struct TeeLogMakeWriter;

impl TeeLogMakeWriter {
    fn new() -> Self {
        Self
    }
}

struct TeeLogWriter {
    stderr: io::Stderr,
    buf: Vec<u8>,
}

impl Write for TeeLogWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        let _ = self.stderr.write_all(&self.buf);
        let _ = self.stderr.flush();
        // Write buffered bytes to the shared file handle under the lock.
        if let Some(Some(mutex)) = LOG_FILE.get()
            && let Ok(mut f) = mutex.lock()
        {
            let _ = f.write_all(&self.buf);
            let _ = f.flush();
        }
        self.buf.clear();
        Ok(())
    }
}

impl Drop for TeeLogWriter {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for TeeLogMakeWriter {
    type Writer = TeeLogWriter;

    fn make_writer(&'a self) -> Self::Writer {
        // Ensure the global file handle is initialised.
        LOG_FILE.get_or_init(open_log_file);
        TeeLogWriter {
            stderr: io::stderr(),
            buf: Vec::with_capacity(256),
        }
    }
}

fn default_log_file_path() -> Option<PathBuf> {
    if let Ok(raw) = std::env::var("HERMES_LOG_FILE") {
        let trimmed = raw.trim();
        if trimmed.is_empty()
            || trimmed.eq_ignore_ascii_case("stderr")
            || trimmed.eq_ignore_ascii_case("none")
            || trimmed.eq_ignore_ascii_case("off")
        {
            return None;
        }
        let path = PathBuf::from(trimmed);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        return Some(path);
    }

    let path = hermes_config::hermes_home().join("logs").join("hermes.log");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    Some(path)
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            level: "info".to_string(),
            json: false,
            service_name: "hermes".to_string(),
            otlp_endpoint: None,
        }
    }
}

/// Build [`TelemetryConfig`] from environment and call [`init_telemetry`].
///
/// Honors `RUST_LOG` / `HERMES_LOG_JSON` / `HERMES_OTLP_ENDPOINT` like the CLI and HTTP binaries.
pub fn init_telemetry_from_env(service_name: impl Into<String>, default_level: impl AsRef<str>) {
    let level = tracing_subscriber::EnvFilter::try_from_default_env()
        .map(|f| f.to_string())
        .unwrap_or_else(|_| default_level.as_ref().to_string());
    let cfg = TelemetryConfig {
        level,
        json: std::env::var("HERMES_LOG_JSON").ok().as_deref() == Some("1"),
        service_name: service_name.into(),
        otlp_endpoint: std::env::var("HERMES_OTLP_ENDPOINT").ok(),
    };
    init_telemetry(&cfg);
}

pub fn init_telemetry(config: &TelemetryConfig) {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(config.level.clone()));

    #[cfg(feature = "otlp")]
    let reg = {
        use tracing_subscriber::layer::Identity;
        let otel: Box<dyn tracing_subscriber::layer::Layer<Registry> + Send + Sync> = match config
            .otlp_endpoint
            .as_deref()
        {
            Some(ep) if !ep.is_empty() => match otlp::build_otel_layer(&config.service_name, ep) {
                Ok(layer) => Box::new(layer),
                Err(e) => {
                    eprintln!("hermes-telemetry: OTLP init failed: {}", e);
                    Box::new(Identity::default())
                }
            },
            _ => Box::new(Identity::default()),
        };
        Registry::default().with(otel)
    };

    #[cfg(not(feature = "otlp"))]
    let reg = {
        if config.otlp_endpoint.is_some() {
            eprintln!(
                "hermes-telemetry: OTLP endpoint is set but this build lacks the `otlp` feature; rebuild with `--features otlp`."
            );
        }
        Registry::default()
    };

    let fmt_base = tracing_subscriber::fmt::layer()
        .event_format(LocalFormatter)
        .with_writer(TeeLogMakeWriter::new());
    let _ = if config.json {
        let json_layer = tracing_subscriber::fmt::layer()
            .json()
            .with_writer(TeeLogMakeWriter::new());
        reg.with(filter).with(json_layer).try_init()
    } else {
        reg.with(filter).with(fmt_base).try_init()
    };
}

#[derive(Default)]
pub struct MetricsRegistry {
    pub llm_requests_total: AtomicU64,
    pub tool_calls_total: AtomicU64,
    pub tool_time_ms_total: AtomicU64,
    pub errors_total: AtomicU64,
    pub http_requests_total: AtomicU64,
    pub http_rejects_total: AtomicU64,
    pub prompt_cache_hits: AtomicU64,
    pub prompt_cache_misses: AtomicU64,
    /// Full user-turn completions through the agent loop.
    pub agent_turns_total: AtomicU64,
    /// LLM round-trip wall time (milliseconds, cumulative).
    pub agent_llm_latency_ms_total: AtomicU64,
    /// Codex app-server turns completed.
    pub agent_codex_turns_total: AtomicU64,
    /// Codex tool iterations observed inside app-server turns.
    pub agent_codex_tool_iterations_total: AtomicU64,
    /// Nous cross-session rate-limit breaker trips.
    pub agent_nous_rate_limit_recorded_total: AtomicU64,
    /// Nous rate-limit pre-call skips (guard hit before HTTP).
    pub agent_nous_rate_limit_skips_total: AtomicU64,
}

pub static METRICS: Lazy<MetricsRegistry> = Lazy::new(MetricsRegistry::default);

pub fn record_llm_request() {
    METRICS.llm_requests_total.fetch_add(1, Ordering::Relaxed);
}

pub fn record_tool_call(duration: Duration) {
    METRICS.tool_calls_total.fetch_add(1, Ordering::Relaxed);
    METRICS
        .tool_time_ms_total
        .fetch_add(duration.as_millis() as u64, Ordering::Relaxed);
}

pub fn record_error() {
    METRICS.errors_total.fetch_add(1, Ordering::Relaxed);
}

pub fn record_http_request() {
    METRICS.http_requests_total.fetch_add(1, Ordering::Relaxed);
}

pub fn record_http_reject() {
    METRICS.http_rejects_total.fetch_add(1, Ordering::Relaxed);
}

pub fn record_prompt_cache_hit() {
    METRICS.prompt_cache_hits.fetch_add(1, Ordering::Relaxed);
}

pub fn record_prompt_cache_miss() {
    METRICS.prompt_cache_misses.fetch_add(1, Ordering::Relaxed);
}

pub fn record_agent_turn() {
    METRICS.agent_turns_total.fetch_add(1, Ordering::Relaxed);
}

pub fn record_llm_latency(duration: Duration) {
    METRICS
        .agent_llm_latency_ms_total
        .fetch_add(duration.as_millis() as u64, Ordering::Relaxed);
}

pub fn record_codex_turn(tool_iterations: u32) {
    METRICS.agent_codex_turns_total.fetch_add(1, Ordering::Relaxed);
    METRICS
        .agent_codex_tool_iterations_total
        .fetch_add(tool_iterations as u64, Ordering::Relaxed);
}

pub fn record_nous_rate_limit_recorded() {
    METRICS
        .agent_nous_rate_limit_recorded_total
        .fetch_add(1, Ordering::Relaxed);
}

pub fn record_nous_rate_limit_skip() {
    METRICS
        .agent_nous_rate_limit_skips_total
        .fetch_add(1, Ordering::Relaxed);
}

#[derive(Debug, Clone, Serialize)]
pub struct MetricsSnapshot {
    pub llm_requests_total: u64,
    pub tool_calls_total: u64,
    pub tool_time_ms_total: u64,
    pub errors_total: u64,
    pub http_requests_total: u64,
    pub http_rejects_total: u64,
    pub prompt_cache_hits: u64,
    pub prompt_cache_misses: u64,
    pub agent_turns_total: u64,
    pub agent_llm_latency_ms_total: u64,
    pub agent_codex_turns_total: u64,
    pub agent_codex_tool_iterations_total: u64,
    pub agent_nous_rate_limit_recorded_total: u64,
    pub agent_nous_rate_limit_skips_total: u64,
}

pub fn snapshot() -> MetricsSnapshot {
    MetricsSnapshot {
        llm_requests_total: METRICS.llm_requests_total.load(Ordering::Relaxed),
        tool_calls_total: METRICS.tool_calls_total.load(Ordering::Relaxed),
        tool_time_ms_total: METRICS.tool_time_ms_total.load(Ordering::Relaxed),
        errors_total: METRICS.errors_total.load(Ordering::Relaxed),
        http_requests_total: METRICS.http_requests_total.load(Ordering::Relaxed),
        http_rejects_total: METRICS.http_rejects_total.load(Ordering::Relaxed),
        prompt_cache_hits: METRICS.prompt_cache_hits.load(Ordering::Relaxed),
        prompt_cache_misses: METRICS.prompt_cache_misses.load(Ordering::Relaxed),
        agent_turns_total: METRICS.agent_turns_total.load(Ordering::Relaxed),
        agent_llm_latency_ms_total: METRICS.agent_llm_latency_ms_total.load(Ordering::Relaxed),
        agent_codex_turns_total: METRICS.agent_codex_turns_total.load(Ordering::Relaxed),
        agent_codex_tool_iterations_total: METRICS
            .agent_codex_tool_iterations_total
            .load(Ordering::Relaxed),
        agent_nous_rate_limit_recorded_total: METRICS
            .agent_nous_rate_limit_recorded_total
            .load(Ordering::Relaxed),
        agent_nous_rate_limit_skips_total: METRICS
            .agent_nous_rate_limit_skips_total
            .load(Ordering::Relaxed),
    }
}

/// OpenMetrics/Prometheus text exposition for scraping (no external `prometheus` crate).
pub fn prometheus_text() -> String {
    let s = snapshot();
    let mut out = String::new();
    use std::fmt::Write;
    let _ = writeln!(
        &mut out,
        "# HELP hermes_llm_requests_total Completed LLM round-trips observed by Hermes."
    );
    let _ = writeln!(&mut out, "# TYPE hermes_llm_requests_total counter");
    let _ = writeln!(
        &mut out,
        "hermes_llm_requests_total {}",
        s.llm_requests_total
    );
    let _ = writeln!(
        &mut out,
        "# HELP hermes_tool_calls_total Tool invocations observed by Hermes."
    );
    let _ = writeln!(&mut out, "# TYPE hermes_tool_calls_total counter");
    let _ = writeln!(&mut out, "hermes_tool_calls_total {}", s.tool_calls_total);
    let _ = writeln!(
        &mut out,
        "# HELP hermes_tool_time_ms_total Wall time spent in tools (milliseconds)."
    );
    let _ = writeln!(&mut out, "# TYPE hermes_tool_time_ms_total counter");
    let _ = writeln!(
        &mut out,
        "hermes_tool_time_ms_total {}",
        s.tool_time_ms_total
    );
    let _ = writeln!(
        &mut out,
        "# HELP hermes_errors_total Errors recorded by Hermes telemetry."
    );
    let _ = writeln!(&mut out, "# TYPE hermes_errors_total counter");
    let _ = writeln!(&mut out, "hermes_errors_total {}", s.errors_total);
    let _ = writeln!(
        &mut out,
        "# HELP hermes_http_requests_total HTTP API requests handled (hermes-http)."
    );
    let _ = writeln!(&mut out, "# TYPE hermes_http_requests_total counter");
    let _ = writeln!(
        &mut out,
        "hermes_http_requests_total {}",
        s.http_requests_total
    );
    let _ = writeln!(
        &mut out,
        "# HELP hermes_http_rejects_total HTTP requests rejected (auth / IP / rate limit)."
    );
    let _ = writeln!(&mut out, "# TYPE hermes_http_rejects_total counter");
    let _ = writeln!(
        &mut out,
        "hermes_http_rejects_total {}",
        s.http_rejects_total
    );
    let _ = writeln!(
        &mut out,
        "# HELP hermes_prompt_cache_hits Prompt cache hits (system prompt unchanged)."
    );
    let _ = writeln!(&mut out, "# TYPE hermes_prompt_cache_hits counter");
    let _ = writeln!(&mut out, "hermes_prompt_cache_hits {}", s.prompt_cache_hits);
    let _ = writeln!(
        &mut out,
        "# HELP hermes_prompt_cache_misses Prompt cache misses (system prompt rebuilt)."
    );
    let _ = writeln!(&mut out, "# TYPE hermes_prompt_cache_misses counter");
    let _ = writeln!(
        &mut out,
        "hermes_prompt_cache_misses {}",
        s.prompt_cache_misses
    );
    let _ = writeln!(
        &mut out,
        "# HELP hermes_agent_turns_total Completed agent user turns."
    );
    let _ = writeln!(&mut out, "# TYPE hermes_agent_turns_total counter");
    let _ = writeln!(&mut out, "hermes_agent_turns_total {}", s.agent_turns_total);
    let _ = writeln!(
        &mut out,
        "# HELP hermes_agent_llm_latency_ms_total Cumulative LLM call latency (ms)."
    );
    let _ = writeln!(&mut out, "# TYPE hermes_agent_llm_latency_ms_total counter");
    let _ = writeln!(
        &mut out,
        "hermes_agent_llm_latency_ms_total {}",
        s.agent_llm_latency_ms_total
    );
    let _ = writeln!(
        &mut out,
        "# HELP hermes_agent_codex_turns_total Codex app-server turns."
    );
    let _ = writeln!(&mut out, "# TYPE hermes_agent_codex_turns_total counter");
    let _ = writeln!(
        &mut out,
        "hermes_agent_codex_turns_total {}",
        s.agent_codex_turns_total
    );
    let _ = writeln!(
        &mut out,
        "# HELP hermes_agent_nous_rate_limit_recorded_total Nous cross-session breaker writes."
    );
    let _ = writeln!(
        &mut out,
        "# TYPE hermes_agent_nous_rate_limit_recorded_total counter"
    );
    let _ = writeln!(
        &mut out,
        "hermes_agent_nous_rate_limit_recorded_total {}",
        s.agent_nous_rate_limit_recorded_total
    );
    let _ = writeln!(
        &mut out,
        "# HELP hermes_agent_nous_rate_limit_skips_total Nous guard pre-call skips."
    );
    let _ = writeln!(
        &mut out,
        "# TYPE hermes_agent_nous_rate_limit_skips_total counter"
    );
    let _ = writeln!(
        &mut out,
        "hermes_agent_nous_rate_limit_skips_total {}",
        s.agent_nous_rate_limit_skips_total
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prometheus_text_includes_counters() {
        record_http_request();
        record_llm_request();
        record_http_reject();
        let t = prometheus_text();
        assert!(t.contains("hermes_http_requests_total"));
        assert!(t.contains("hermes_llm_requests_total"));
        assert!(t.contains("hermes_http_rejects_total"));
    }
}
