//! Parallel meta-search orchestration for `DdgsSearchBackend`.

use std::time::{Duration, Instant};

use reqwest::Client;
use serde_json::json;
use tokio::task::JoinSet;
use tracing::{debug, info};

use super::cn::{bing_cn::BingCnEngine, engine::run_cn_engine, sogou::SogouEngine};
use super::config::{CnEngineKind, MetaSearchConfig};
use super::ddgs::ddgs_search_with_fallback;
use super::http_client::build_meta_search_client;
use super::merge::merge_and_rank;
use super::query_locale::query_has_cjk;
use super::{EngineAttempt, SearchHit};
use hermes_core::ToolError;

struct EngineOutcome {
    hits: Vec<SearchHit>,
    attempt: EngineAttempt,
}

/// Run meta-search and return the legacy DDGS JSON envelope.
pub async fn meta_search(query: &str, num_results: usize) -> Result<String, ToolError> {
    let cfg = MetaSearchConfig::from_env();
    let limit = num_results.max(1);
    let use_cn = query_has_cjk(query) && !cfg.cn_engines.is_empty();

    if !use_cn {
        return finish_ddgs_only(query, limit).await;
    }

    let global_timeout = Duration::from_secs(cfg.global_timeout_secs.max(1));
    let client = build_meta_search_client(cfg.cn_timeout_secs);
    let base_override = cfg.cn_base_url_override.as_deref();
    let query_owned = query.to_string();

    let work = async move {
        let mut set = JoinSet::new();
        if !ddgs_disabled() {
            let q = query_owned.clone();
            set.spawn(async move {
                run_ddgs_task(q, limit).await
            });
        }

        for kind in cfg.cn_engines {
            let client = client.clone();
            let query = query_owned.clone();
            let cn_timeout = Duration::from_secs(cfg.cn_timeout_secs);
            let base = base_override.map(str::to_string);
            set.spawn(async move {
                run_cn_task(kind, client, &query, limit, cn_timeout, base.as_deref()).await
            });
        }

        let mut outcomes: Vec<EngineOutcome> = Vec::new();
        while let Some(joined) = set.join_next().await {
            match joined {
                Ok(outcome) => outcomes.push(outcome),
                Err(e) => {
                    debug!(error = %e, "meta_search task panic");
                }
            }
        }
        outcomes
    };

    let outcomes = match tokio::time::timeout(global_timeout, work).await {
        Ok(v) => v,
        Err(_) => {
            debug!(
                timeout_secs = global_timeout.as_secs(),
                "meta_search global deadline exceeded"
            );
            Vec::new()
        }
    };

    let attempts: Vec<EngineAttempt> = outcomes.iter().map(|o| o.attempt.clone()).collect();
    let batches: Vec<Vec<SearchHit>> = outcomes.into_iter().map(|o| o.hits).collect();
    let merged = merge_and_rank(batches, limit, true, cfg.cn_weight);

    if merged.is_empty() {
        let msg = if attempts.is_empty() {
            "Meta search timed out before any engine returned results.".to_string()
        } else {
            "Meta search returned no results from configured engines.".to_string()
        };
        return serde_json::to_string(&json!({
            "success": false,
            "error": msg,
            "_trace": { "attempts": attempts },
        }))
        .map_err(|e| ToolError::ExecutionFailed(e.to_string()));
    }

    info!(
        query_chars = query.chars().count(),
        result_count = merged.len(),
        "meta_search CJK query complete"
    );
    Ok(serialize_success(query, merged, attempts))
}

async fn finish_ddgs_only(query: &str, limit: usize) -> Result<String, ToolError> {
    let started = Instant::now();
    match ddgs_search_with_fallback(query, limit).await {
        Ok(hits) => {
            let attempt = EngineAttempt {
                engine: "ddgs".into(),
                status: "ok".into(),
                duration_ms: started.elapsed().as_millis() as u64,
                count: Some(hits.len()),
                error: None,
            };
            Ok(serialize_success(query, hits, vec![attempt]))
        }
        Err(err) => {
            let attempt = EngineAttempt {
                engine: "ddgs".into(),
                status: "error".into(),
                duration_ms: started.elapsed().as_millis() as u64,
                count: None,
                error: Some(truncate_err(&err.to_string(), 250)),
            };
            Ok(serde_json::to_string(&json!({
                "success": false,
                "error": err.to_string(),
                "_trace": { "attempts": [attempt] },
            }))
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?)
        }
    }
}

async fn run_ddgs_task(query: String, limit: usize) -> EngineOutcome {
    let started = Instant::now();
    match ddgs_search_with_fallback(&query, limit).await {
        Ok(hits) => {
            let count = hits.len();
            EngineOutcome {
                hits,
                attempt: EngineAttempt {
                    engine: "ddgs".into(),
                    status: if count == 0 { "empty" } else { "ok" }.into(),
                    duration_ms: started.elapsed().as_millis() as u64,
                    count: Some(count),
                    error: None,
                },
            }
        }
        Err(e) => EngineOutcome {
            hits: Vec::new(),
            attempt: EngineAttempt {
                engine: "ddgs".into(),
                status: "error".into(),
                duration_ms: started.elapsed().as_millis() as u64,
                count: None,
                error: Some(truncate_err(&e.to_string(), 250)),
            },
        },
    }
}

async fn run_cn_task(
    kind: CnEngineKind,
    client: Client,
    query: &str,
    limit: usize,
    timeout: Duration,
    base_override: Option<&str>,
) -> EngineOutcome {
    let engine_id = kind.id();
    let started = Instant::now();
    let engine: &dyn crate::backends::meta_search::cn::engine::CnHtmlEngine = match kind {
        CnEngineKind::Sogou => &SogouEngine,
        CnEngineKind::BingCn => &BingCnEngine,
    };

    let fut = run_cn_engine(engine, &client, query, limit, base_override);
    match tokio::time::timeout(timeout, fut).await {
        Ok(Ok(hits)) => {
            let count = hits.len();
            EngineOutcome {
                hits,
                attempt: EngineAttempt {
                    engine: engine_id.into(),
                    status: if count == 0 { "empty" } else { "ok" }.into(),
                    duration_ms: started.elapsed().as_millis() as u64,
                    count: Some(count),
                    error: None,
                },
            }
        }
        Ok(Err(e)) => EngineOutcome {
            hits: Vec::new(),
            attempt: EngineAttempt {
                engine: engine_id.into(),
                status: "error".into(),
                duration_ms: started.elapsed().as_millis() as u64,
                count: None,
                error: Some(truncate_err(&e.to_string(), 250)),
            },
        },
        Err(_) => EngineOutcome {
            hits: Vec::new(),
            attempt: EngineAttempt {
                engine: engine_id.into(),
                status: "error".into(),
                duration_ms: started.elapsed().as_millis() as u64,
                count: None,
                error: Some(format!("timed out after {}s", timeout.as_secs())),
            },
        },
    }
}

fn serialize_success(query: &str, hits: Vec<SearchHit>, attempts: Vec<EngineAttempt>) -> String {
    let web: Vec<serde_json::Value> = hits
        .into_iter()
        .enumerate()
        .map(|(i, h)| {
            json!({
                "title": h.title,
                "url": h.url,
                "description": h.description,
                "position": i + 1,
                "source": h.source,
            })
        })
        .collect();
    serde_json::to_string(&json!({
        "success": true,
        "data": { "web": web },
        "query": query,
        "_trace": { "attempts": attempts },
    }))
    .unwrap_or_else(|_| r#"{"success":false,"error":"serialize failed"}"#.into())
}

fn ddgs_disabled() -> bool {
    std::env::var("HERMES_META_SEARCH_DDGS_DISABLED")
        .ok()
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn truncate_err(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max).collect::<String>())
    }
}
