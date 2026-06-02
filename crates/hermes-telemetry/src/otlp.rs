//! OTLP HTTP trace export (`otlp` Cargo feature).

use std::collections::HashMap;

use opentelemetry::global;
use opentelemetry::KeyValue;
use opentelemetry_otlp::{WithExportConfig, WithHttpConfig};
use opentelemetry_sdk::trace::{Sampler, SdkTracerProvider};
use opentelemetry_sdk::Resource;
use tracing_subscriber::Layer;
use tracing_subscriber::Registry;

/// Build a `tracing` layer that exports spans to an OTLP/HTTP collector.
pub fn build_otel_layer(
    service_name: &str,
    endpoint: &str,
    headers: &[(String, String)],
    sample_rate: Option<f64>,
    resource_attributes: &[(String, String)],
) -> Result<impl Layer<Registry> + Send + Sync, Box<dyn std::error::Error + Send + Sync + 'static>>
{
    let endpoint = normalize_traces_endpoint(endpoint);
    let headers = headers.iter().cloned().collect::<HashMap<_, _>>();

    let mut exporter_builder = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_endpoint(endpoint);
    if !headers.is_empty() {
        exporter_builder = exporter_builder.with_headers(headers);
    }
    let exporter = exporter_builder.build()?;

    let mut resource_builder =
        Resource::builder_empty().with_service_name(service_name.to_string());
    for (key, value) in resource_attributes {
        resource_builder =
            resource_builder.with_attribute(KeyValue::new(key.clone(), value.clone()));
    }
    let resource = resource_builder.build();

    let mut provider_builder = SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(resource);
    if let Some(sample_rate) = sample_rate {
        provider_builder =
            provider_builder.with_sampler(Sampler::TraceIdRatioBased(sample_rate.clamp(0.0, 1.0)));
    }
    let provider = provider_builder.build();

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
