//! Telemetry bootstrap and in-process metrics registry.

use base64::Engine;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use tracing_subscriber::prelude::*;
use tracing_subscriber::Registry;

#[cfg(feature = "otlp")]
mod otlp;

const DEFAULT_LANGFUSE_BASE_URL: &str = "https://cloud.langfuse.com";
const LANGFUSE_PUBLIC_KEY_PREFIX: &str = "pk-lf-";
const LANGFUSE_SECRET_KEY_PREFIX: &str = "sk-lf-";
const LANGFUSE_INGESTION_VERSION: &str = "4";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryConfig {
    pub level: String,
    pub json: bool,
    pub service_name: String,
    /// OTLP HTTP traces URL base (`http://host:4318`) or full path (`.../v1/traces`).
    /// Active when crate is built with `--features otlp`.
    pub otlp_endpoint: Option<String>,
    /// Additional OTLP HTTP headers. Used for Langfuse Basic auth and supported
    /// for explicit OTLP endpoints through `HERMES_OTLP_HEADERS`.
    #[serde(default)]
    pub otlp_headers: Vec<(String, String)>,
    /// Optional trace-id-ratio sampler. Values are clamped to `0.0..=1.0`.
    #[serde(default)]
    pub otlp_sample_rate: Option<f64>,
    /// Resource attributes attached to exported OTLP traces.
    #[serde(default)]
    pub resource_attributes: Vec<(String, String)>,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            level: "info".to_string(),
            json: false,
            service_name: "hermes".to_string(),
            otlp_endpoint: None,
            otlp_headers: Vec::new(),
            otlp_sample_rate: None,
            resource_attributes: Vec::new(),
        }
    }
}

#[derive(Clone, PartialEq)]
pub struct LangfuseTraceConfig {
    pub public_key: String,
    secret_key: String,
    pub base_url: String,
    pub endpoint: String,
    pub environment: Option<String>,
    pub release: Option<String>,
    pub sample_rate: Option<f64>,
}

impl std::fmt::Debug for LangfuseTraceConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LangfuseTraceConfig")
            .field("public_key", &redact_key_preview(&self.public_key))
            .field("secret_key", &"<redacted>")
            .field("base_url", &self.base_url)
            .field("endpoint", &self.endpoint)
            .field("environment", &self.environment)
            .field("release", &self.release)
            .field("sample_rate", &self.sample_rate)
            .finish()
    }
}

impl LangfuseTraceConfig {
    pub fn otlp_headers(&self) -> Vec<(String, String)> {
        vec![
            (
                "Authorization".to_string(),
                format!("Basic {}", self.basic_auth_token()),
            ),
            (
                "x-langfuse-ingestion-version".to_string(),
                LANGFUSE_INGESTION_VERSION.to_string(),
            ),
        ]
    }

    pub fn resource_attributes(&self) -> Vec<(String, String)> {
        let mut attrs = vec![("hermes.observability.provider".into(), "langfuse".into())];
        if let Some(environment) = &self.environment {
            attrs.push(("deployment.environment.name".into(), environment.clone()));
            attrs.push(("langfuse.environment".into(), environment.clone()));
        }
        if let Some(release) = &self.release {
            attrs.push(("service.version".into(), release.clone()));
            attrs.push(("langfuse.release".into(), release.clone()));
        }
        attrs
    }

    fn basic_auth_token(&self) -> String {
        base64::engine::general_purpose::STANDARD
            .encode(format!("{}:{}", self.public_key, self.secret_key))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LangfuseConfigError {
    pub issues: Vec<String>,
}

impl std::fmt::Display for LangfuseConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.issues.join("; "))
    }
}

impl std::error::Error for LangfuseConfigError {}

/// Build [`TelemetryConfig`] from environment and call [`init_telemetry`].
///
/// Honors `RUST_LOG`, `HERMES_LOG_JSON`, explicit OTLP env, and
/// `HERMES_LANGFUSE_*`/`LANGFUSE_*` credentials. Langfuse maps to its native
/// OTLP/HTTP traces endpoint (`/api/public/otel/v1/traces`) and uses Basic auth.
pub fn init_telemetry_from_env(service_name: impl Into<String>, default_level: impl AsRef<str>) {
    let level = tracing_subscriber::EnvFilter::try_from_default_env()
        .map(|f| f.to_string())
        .unwrap_or_else(|_| default_level.as_ref().to_string());
    let trace_export = trace_export_from_env();
    let cfg = TelemetryConfig {
        level,
        json: std::env::var("HERMES_LOG_JSON").ok().as_deref() == Some("1"),
        service_name: service_name.into(),
        otlp_endpoint: trace_export.endpoint,
        otlp_headers: trace_export.headers,
        otlp_sample_rate: trace_export.sample_rate,
        resource_attributes: trace_export.resource_attributes,
    };
    init_telemetry(&cfg);
}

pub fn init_telemetry(config: &TelemetryConfig) {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(config.level.clone()));

    #[cfg(feature = "otlp")]
    let reg = {
        use tracing_subscriber::layer::Identity;
        let otel: Box<dyn tracing_subscriber::layer::Layer<Registry> + Send + Sync> =
            match config.otlp_endpoint.as_deref() {
                Some(ep) if !ep.is_empty() => match otlp::build_otel_layer(
                    &config.service_name,
                    ep,
                    &config.otlp_headers,
                    config.otlp_sample_rate,
                    &config.resource_attributes,
                ) {
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
    pub prompt_cache_hits: AtomicU64,
    pub prompt_cache_misses: AtomicU64,
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
    out
}

#[derive(Default)]
struct TraceExport {
    endpoint: Option<String>,
    headers: Vec<(String, String)>,
    sample_rate: Option<f64>,
    resource_attributes: Vec<(String, String)>,
}

fn trace_export_from_env() -> TraceExport {
    let explicit_endpoint = first_env([
        "HERMES_OTLP_ENDPOINT",
        "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
        "OTEL_EXPORTER_OTLP_ENDPOINT",
    ]);

    if let Some(endpoint) = explicit_endpoint {
        return TraceExport {
            endpoint: Some(endpoint),
            headers: first_env(["HERMES_OTLP_HEADERS", "OTEL_EXPORTER_OTLP_HEADERS"])
                .map(|raw| parse_otlp_headers(&raw))
                .unwrap_or_default(),
            sample_rate: first_env(["HERMES_OTLP_SAMPLE_RATE", "OTEL_TRACES_SAMPLER_ARG"])
                .and_then(|raw| parse_sample_rate(&raw)),
            resource_attributes: Vec::new(),
        };
    }

    match langfuse_trace_config_from_env() {
        Ok(Some(langfuse)) => TraceExport {
            endpoint: Some(langfuse.endpoint.clone()),
            headers: langfuse.otlp_headers(),
            sample_rate: langfuse.sample_rate,
            resource_attributes: langfuse.resource_attributes(),
        },
        Ok(None) => TraceExport::default(),
        Err(err) => {
            eprintln!(
                "hermes-telemetry: Langfuse disabled because credentials are invalid: {}",
                err
            );
            TraceExport::default()
        }
    }
}

pub fn langfuse_trace_config_from_env() -> Result<Option<LangfuseTraceConfig>, LangfuseConfigError>
{
    resolve_langfuse_trace_config(|name| std::env::var(name).ok())
}

fn resolve_langfuse_trace_config<F>(
    mut get_env: F,
) -> Result<Option<LangfuseTraceConfig>, LangfuseConfigError>
where
    F: FnMut(&str) -> Option<String>,
{
    let public_key = first_env_from(
        &mut get_env,
        ["HERMES_LANGFUSE_PUBLIC_KEY", "LANGFUSE_PUBLIC_KEY"],
    );
    let secret_key = first_env_from(
        &mut get_env,
        ["HERMES_LANGFUSE_SECRET_KEY", "LANGFUSE_SECRET_KEY"],
    );

    let (public_key, secret_key) = match (public_key, secret_key) {
        (None, None) => return Ok(None),
        (Some(public_key), Some(secret_key)) => (public_key, secret_key),
        (Some(_), None) => {
            return Err(LangfuseConfigError {
                issues: vec!["missing HERMES_LANGFUSE_SECRET_KEY/LANGFUSE_SECRET_KEY".into()],
            })
        }
        (None, Some(_)) => {
            return Err(LangfuseConfigError {
                issues: vec!["missing HERMES_LANGFUSE_PUBLIC_KEY/LANGFUSE_PUBLIC_KEY".into()],
            })
        }
    };

    let mut issues = Vec::new();
    if !public_key.starts_with(LANGFUSE_PUBLIC_KEY_PREFIX) {
        issues.push(format!(
            "HERMES_LANGFUSE_PUBLIC_KEY={} (expected {:?} prefix)",
            redact_key_preview(&public_key),
            LANGFUSE_PUBLIC_KEY_PREFIX
        ));
    }
    if !secret_key.starts_with(LANGFUSE_SECRET_KEY_PREFIX) {
        issues.push(format!(
            "HERMES_LANGFUSE_SECRET_KEY={} (expected {:?} prefix)",
            redact_key_preview(&secret_key),
            LANGFUSE_SECRET_KEY_PREFIX
        ));
    }
    if !issues.is_empty() {
        return Err(LangfuseConfigError { issues });
    }

    let base_url = first_env_from(
        &mut get_env,
        [
            "HERMES_LANGFUSE_BASE_URL",
            "LANGFUSE_BASE_URL",
            "LANGFUSE_HOST",
        ],
    )
    .unwrap_or_else(|| DEFAULT_LANGFUSE_BASE_URL.to_string());
    let endpoint = langfuse_otlp_traces_endpoint(&base_url);
    let environment = first_env_from(&mut get_env, ["HERMES_LANGFUSE_ENV", "LANGFUSE_ENV"]);
    let release = first_env_from(
        &mut get_env,
        ["HERMES_LANGFUSE_RELEASE", "LANGFUSE_RELEASE"],
    );
    let sample_rate = first_env_from(
        &mut get_env,
        ["HERMES_LANGFUSE_SAMPLE_RATE", "LANGFUSE_SAMPLE_RATE"],
    )
    .and_then(|raw| parse_sample_rate(&raw));

    Ok(Some(LangfuseTraceConfig {
        public_key,
        secret_key,
        base_url,
        endpoint,
        environment,
        release,
        sample_rate,
    }))
}

fn first_env<const N: usize>(names: [&str; N]) -> Option<String> {
    let mut get = |name: &str| std::env::var(name).ok();
    first_env_from(&mut get, names)
}

fn first_env_from<F, const N: usize>(get_env: &mut F, names: [&str; N]) -> Option<String>
where
    F: FnMut(&str) -> Option<String>,
{
    names.into_iter().find_map(|name| {
        get_env(name).and_then(|value| {
            let trimmed = value.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        })
    })
}

fn langfuse_otlp_traces_endpoint(base_url: &str) -> String {
    let trimmed = base_url.trim().trim_end_matches('/');
    if trimmed.ends_with("/api/public/otel/v1/traces") {
        trimmed.to_string()
    } else if trimmed.ends_with("/api/public/otel") {
        format!("{trimmed}/v1/traces")
    } else {
        format!("{trimmed}/api/public/otel/v1/traces")
    }
}

fn parse_otlp_headers(raw: &str) -> Vec<(String, String)> {
    raw.split(',')
        .filter_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            let key = key.trim();
            let value = value.trim();
            (!key.is_empty() && !value.is_empty()).then(|| (key.to_string(), value.to_string()))
        })
        .collect()
}

fn parse_sample_rate(raw: &str) -> Option<f64> {
    raw.trim()
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())
        .map(|value| value.clamp(0.0, 1.0))
}

fn redact_key_preview(value: &str) -> String {
    if value.is_empty() {
        return "<empty>".into();
    }
    if value.len() <= 12 {
        return format!("{value:?}");
    }
    let prefix = value.chars().take(6).collect::<String>();
    format!("{:?}", format!("{prefix}..."))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn map_env(entries: &[(&str, &str)]) -> HashMap<String, String> {
        entries
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect()
    }

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

    #[test]
    fn langfuse_config_resolves_endpoint_headers_and_resource_attrs() {
        let env = map_env(&[
            ("HERMES_LANGFUSE_PUBLIC_KEY", "pk-lf-public"),
            ("HERMES_LANGFUSE_SECRET_KEY", "sk-lf-secret"),
            ("HERMES_LANGFUSE_BASE_URL", "https://us.cloud.langfuse.com/"),
            ("HERMES_LANGFUSE_ENV", "local"),
            ("HERMES_LANGFUSE_RELEASE", "v1.2.3"),
            ("HERMES_LANGFUSE_SAMPLE_RATE", "0.25"),
        ]);
        let cfg = resolve_langfuse_trace_config(|name| env.get(name).cloned())
            .expect("valid langfuse config")
            .expect("configured");

        assert_eq!(
            cfg.endpoint,
            "https://us.cloud.langfuse.com/api/public/otel/v1/traces"
        );
        assert_eq!(cfg.sample_rate, Some(0.25));
        let headers = cfg.otlp_headers();
        assert!(headers
            .iter()
            .any(|(k, v)| k == "Authorization" && v.starts_with("Basic ")));
        assert!(headers.iter().any(|(k, v)| {
            k == "x-langfuse-ingestion-version" && v == LANGFUSE_INGESTION_VERSION
        }));
        assert!(cfg
            .resource_attributes()
            .contains(&("deployment.environment.name".into(), "local".into())));
        assert!(cfg
            .resource_attributes()
            .contains(&("service.version".into(), "v1.2.3".into())));
    }

    #[test]
    fn langfuse_config_rejects_missing_or_placeholder_credentials() {
        let env = map_env(&[
            ("HERMES_LANGFUSE_PUBLIC_KEY", "placeholder"),
            ("HERMES_LANGFUSE_SECRET_KEY", "test-secret"),
        ]);
        let err = resolve_langfuse_trace_config(|name| env.get(name).cloned())
            .expect_err("placeholder credentials rejected");
        assert!(err
            .issues
            .iter()
            .any(|issue| issue.contains("expected \"pk-lf-\" prefix")));
        assert!(err
            .issues
            .iter()
            .any(|issue| issue.contains("expected \"sk-lf-\" prefix")));

        let env = map_env(&[("HERMES_LANGFUSE_PUBLIC_KEY", "pk-lf-public")]);
        let err = resolve_langfuse_trace_config(|name| env.get(name).cloned())
            .expect_err("half config rejected");
        assert!(err
            .to_string()
            .contains("missing HERMES_LANGFUSE_SECRET_KEY"));
    }

    #[test]
    fn langfuse_endpoint_normalizes_full_otel_paths() {
        assert_eq!(
            langfuse_otlp_traces_endpoint("https://cloud.langfuse.com"),
            "https://cloud.langfuse.com/api/public/otel/v1/traces"
        );
        assert_eq!(
            langfuse_otlp_traces_endpoint("https://cloud.langfuse.com/api/public/otel"),
            "https://cloud.langfuse.com/api/public/otel/v1/traces"
        );
        assert_eq!(
            langfuse_otlp_traces_endpoint("https://cloud.langfuse.com/api/public/otel/v1/traces"),
            "https://cloud.langfuse.com/api/public/otel/v1/traces"
        );
    }

    #[test]
    fn sample_rate_and_otlp_header_parsing_are_tolerant() {
        assert_eq!(parse_sample_rate("1.5"), Some(1.0));
        assert_eq!(parse_sample_rate("-2"), Some(0.0));
        assert_eq!(parse_sample_rate("nan"), None);
        assert_eq!(parse_sample_rate("garbage"), None);

        assert_eq!(
            parse_otlp_headers("Authorization=Basic abc, x-langfuse-ingestion-version=4,broken"),
            vec![
                ("Authorization".into(), "Basic abc".into()),
                ("x-langfuse-ingestion-version".into(), "4".into())
            ]
        );
    }

    #[test]
    fn explicit_otlp_endpoint_takes_precedence_over_langfuse() {
        let _guard = EnvGuard::set_many(&[
            ("HERMES_OTLP_ENDPOINT", "http://collector:4318"),
            ("HERMES_OTLP_HEADERS", "Authorization=Bearer direct"),
            ("HERMES_LANGFUSE_PUBLIC_KEY", "pk-lf-public"),
            ("HERMES_LANGFUSE_SECRET_KEY", "sk-lf-secret"),
        ]);

        let export = trace_export_from_env();
        assert_eq!(export.endpoint.as_deref(), Some("http://collector:4318"));
        assert_eq!(
            export.headers,
            vec![("Authorization".into(), "Bearer direct".into())]
        );
        assert!(export.resource_attributes.is_empty());
    }

    struct EnvGuard {
        originals: Vec<(&'static str, Option<String>)>,
    }

    impl EnvGuard {
        fn set_many(entries: &[(&'static str, &'static str)]) -> Self {
            let keys = [
                "HERMES_OTLP_ENDPOINT",
                "HERMES_OTLP_HEADERS",
                "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
                "OTEL_EXPORTER_OTLP_ENDPOINT",
                "OTEL_EXPORTER_OTLP_HEADERS",
                "HERMES_LANGFUSE_PUBLIC_KEY",
                "HERMES_LANGFUSE_SECRET_KEY",
                "HERMES_LANGFUSE_BASE_URL",
                "HERMES_LANGFUSE_ENV",
                "HERMES_LANGFUSE_RELEASE",
                "HERMES_LANGFUSE_SAMPLE_RATE",
                "LANGFUSE_PUBLIC_KEY",
                "LANGFUSE_SECRET_KEY",
                "LANGFUSE_BASE_URL",
                "LANGFUSE_HOST",
                "LANGFUSE_ENV",
                "LANGFUSE_RELEASE",
                "LANGFUSE_SAMPLE_RATE",
            ];
            let originals = keys
                .into_iter()
                .map(|key| (key, std::env::var(key).ok()))
                .collect::<Vec<_>>();
            for key in keys {
                std::env::remove_var(key);
            }
            for (key, value) in entries {
                std::env::set_var(key, value);
            }
            Self { originals }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (key, value) in &self.originals {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }
}
