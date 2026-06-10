//! DDGS international search chain (serial fallback).

use std::time::Duration;

use hermes_core::ToolError;

use super::SearchHit;

pub fn ddgs_http_timeout_secs() -> u64 {
    std::env::var("HERMES_DDGS_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(8)
        .min(30)
}

pub fn ddgs_region_from_env() -> ddgs::Region {
    match std::env::var("HERMES_DDGS_REGION")
        .ok()
        .map(|v| v.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("cn-zh" | "cn" | "zh-cn" | "zh") => ddgs::Region::CnZh,
        Some("us-en" | "us") => ddgs::Region::UsEn,
        Some("jp-jp" | "jp") => ddgs::Region::JpJp,
        Some("wt-wt" | "world") => ddgs::Region::WtWt,
        _ => ddgs::Region::CnZh,
    }
}

pub fn parse_ddgs_text_backend(name: &str) -> Option<ddgs::TextBackend> {
    match name.trim().to_ascii_lowercase().as_str() {
        "lite" => Some(ddgs::TextBackend::Lite),
        "html" => Some(ddgs::TextBackend::Html),
        "api" => Some(ddgs::TextBackend::Api),
        "yandex" => Some(ddgs::TextBackend::Yandex),
        "mojeek" => Some(ddgs::TextBackend::Mojeek),
        "yahoo" => Some(ddgs::TextBackend::Yahoo),
        "brave" => Some(ddgs::TextBackend::Brave),
        "google" => Some(ddgs::TextBackend::Google),
        "startpage" => Some(ddgs::TextBackend::Startpage),
        "wikipedia" => Some(ddgs::TextBackend::Wikipedia),
        "auto" => Some(ddgs::TextBackend::Auto),
        "all" => Some(ddgs::TextBackend::All),
        _ => None,
    }
}

pub fn ddgs_backend_priority() -> Vec<ddgs::TextBackend> {
    if let Ok(raw) = std::env::var("HERMES_DDGS_BACKENDS") {
        let parsed: Vec<ddgs::TextBackend> = raw
            .split(',')
            .filter_map(parse_ddgs_text_backend)
            .collect();
        if !parsed.is_empty() {
            return parsed;
        }
    }
    vec![
        ddgs::TextBackend::Lite,
        ddgs::TextBackend::Html,
        ddgs::TextBackend::Yandex,
        ddgs::TextBackend::Mojeek,
    ]
}

fn ddgs_client_from_env() -> Result<ddgs::Ddgs, ToolError> {
    ddgs::Ddgs::new().map_err(|e| ToolError::ExecutionFailed(e.to_string()))
}

/// Run DDGS backends in serial fallback; returns hits tagged with `ddgs_<backend>` source.
pub async fn ddgs_search_with_fallback(
    query: &str,
    num_results: usize,
) -> Result<Vec<SearchHit>, ToolError> {
    let client = ddgs_client_from_env()?;
    let region = ddgs_region_from_env();
    let limit = num_results.max(1);
    let per_backend_timeout = Duration::from_secs(ddgs_http_timeout_secs());
    let backends = ddgs_backend_priority();
    let mut last_error: Option<String> = None;

    for backend in backends {
        let source = format!("ddgs_{}", backend.as_str());
        let opts = ddgs::TextOptions::default()
            .max_results(limit)
            .backend(backend)
            .region(region);
        let search = client.text_with_options(query, opts);
        match tokio::time::timeout(per_backend_timeout, search).await {
            Ok(Ok(hits)) if !hits.is_empty() => {
                tracing::debug!(
                    backend = backend.as_str(),
                    result_count = hits.len(),
                    "DDGS backend returned results"
                );
                return Ok(hits
                    .into_iter()
                    .map(|h| {
                        SearchHit::new(h.title, h.href, h.body, source.clone())
                    })
                    .collect());
            }
            Ok(Ok(_)) => {
                tracing::debug!(
                    backend = backend.as_str(),
                    "DDGS backend returned no results"
                );
            }
            Ok(Err(e)) => {
                tracing::debug!(
                    backend = backend.as_str(),
                    error = %e,
                    "DDGS backend failed"
                );
                last_error = Some(e.to_string());
            }
            Err(_) => {
                tracing::debug!(
                    backend = backend.as_str(),
                    timeout_secs = per_backend_timeout.as_secs(),
                    "DDGS backend timed out"
                );
                last_error = Some(format!(
                    "timed out after {}s",
                    per_backend_timeout.as_secs()
                ));
            }
        }
    }

    Err(ToolError::ExecutionFailed(format!(
        "DuckDuckGo search failed after trying {} backend(s): {}",
        ddgs_backend_priority().len(),
        last_error.unwrap_or_else(|| "no results".into())
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_priority_defaults_to_fast_backends() {
        hermes_core::test_env::remove_var("HERMES_DDGS_BACKENDS");
        let backends = ddgs_backend_priority();
        assert_eq!(backends.len(), 4);
        assert_eq!(backends[0], ddgs::TextBackend::Lite);
        assert_eq!(backends[1], ddgs::TextBackend::Html);
    }

    #[test]
    fn backend_priority_parses_env_list() {
        hermes_core::test_env::set_var("HERMES_DDGS_BACKENDS", "lite,yandex,not-a-backend");
        let backends = ddgs_backend_priority();
        assert_eq!(backends.len(), 2);
        assert_eq!(backends[0], ddgs::TextBackend::Lite);
        assert_eq!(backends[1], ddgs::TextBackend::Yandex);
        hermes_core::test_env::remove_var("HERMES_DDGS_BACKENDS");
    }

    #[test]
    fn region_cn_default() {
        hermes_core::test_env::remove_var("HERMES_DDGS_REGION");
        assert_eq!(ddgs_region_from_env(), ddgs::Region::CnZh);
    }

    #[test]
    fn timeout_default_is_eight_seconds() {
        hermes_core::test_env::remove_var("HERMES_DDGS_TIMEOUT_SECS");
        assert_eq!(ddgs_http_timeout_secs(), 8);
    }
}
