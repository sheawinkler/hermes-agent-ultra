//! Telemetry bootstrap and in-process metrics registry.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use tracing_subscriber::prelude::*;
use tracing_subscriber::Registry;

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

    let fmt_base = tracing_subscriber::fmt::layer().with_target(false);
    let _ = if config.json {
        reg.with(filter).with(fmt_base.json()).try_init()
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

#[derive(Debug, Clone, Serialize)]
pub struct MetricsSnapshot {
    pub llm_requests_total: u64,
    pub tool_calls_total: u64,
    pub tool_time_ms_total: u64,
    pub errors_total: u64,
    pub http_requests_total: u64,
    pub http_rejects_total: u64,
}

pub fn snapshot() -> MetricsSnapshot {
    MetricsSnapshot {
        llm_requests_total: METRICS.llm_requests_total.load(Ordering::Relaxed),
        tool_calls_total: METRICS.tool_calls_total.load(Ordering::Relaxed),
        tool_time_ms_total: METRICS.tool_time_ms_total.load(Ordering::Relaxed),
        errors_total: METRICS.errors_total.load(Ordering::Relaxed),
        http_requests_total: METRICS.http_requests_total.load(Ordering::Relaxed),
        http_rejects_total: METRICS.http_rejects_total.load(Ordering::Relaxed),
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
