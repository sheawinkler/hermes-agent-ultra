//! OTLP HTTP trace export (`otlp` Cargo feature).

use opentelemetry::global;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::trace::SdkTracerProvider;
use tracing_subscriber::Layer;
use tracing_subscriber::Registry;

/// Build a `tracing` layer that exports spans to an OTLP/HTTP collector.
pub fn build_otel_layer(
    service_name: &str,
    endpoint: &str,
) -> Result<impl Layer<Registry> + Send + Sync, Box<dyn std::error::Error + Send + Sync + 'static>>
{
    let endpoint = normalize_traces_endpoint(endpoint);

    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_endpoint(endpoint)
        .build()?;

    let resource = Resource::builder_empty()
        .with_service_name(service_name.to_string())
        .build();

    let provider = SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(resource)
        .build();

    global::set_tracer_provider(provider);
    let tracer = global::tracer(service_name.to_string());
    Ok(tracing_opentelemetry::layer().with_tracer(tracer))
}

/// Accepts `http://host:4318` or full `http://host:4318/v1/traces`.
fn normalize_traces_endpoint(raw: &str) -> String {
    let t = raw.trim();
    if t.contains("/v1/traces") {
        t.to_string()
    } else {
        format!("{}/v1/traces", t.trim_end_matches('/'))
    }
}
