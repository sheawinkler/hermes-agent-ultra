//! Shared POI ingestion helpers (agent loop + memory plugin).

use std::sync::{Arc, Mutex};

use hermes_config::InterestConfig;
use hermes_intelligence::auxiliary::AuxiliaryClient;
use serde_json::Value;
use tracing::info;

use super::extract::{
    extract_signals_from_messages, extract_signals_from_text, filter_poi_signals,
    message_text_from_value,
};
use super::types::ExtractOptions;
use super::llm::extract_signals_from_transcript_llm;
use super::pipeline::apply_signal_batch;
use super::quality::filter_persistable_signals;
use super::store::{InterestSignal, InterestStore};

/// Agent-injected continuation / nudge user lines — not real user POI.
pub fn is_poi_synthetic_user_text(text: &str) -> bool {
    let t = text.trim();
    t.starts_with("[System:")
        && (t.contains("Continue now")
            || t.contains("incomplete due to generation limits")
            || t.contains("Continue exactly where you left off"))
}

/// Rule-based ingest from a single user message (legacy per-turn persist path).
pub fn ingest_user_message(
    store: &InterestStore,
    config: &InterestConfig,
    user_text: &str,
    weight_scale: f64,
) -> Result<(), String> {
    let trimmed = user_text.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    let signals = filter_persistable_signals(filter_poi_signals(extract_signals_from_text(
        trimmed,
        weight_scale,
        ExtractOptions {
            include_keywords: false,
        },
    )));
    if signals.is_empty() {
        return Ok(());
    }
    apply_signal_batch(store, config, signals)?;
    Ok(())
}

/// Session-end ingest: buffered turn signals + rules + optional LLM → compare → update.
pub fn spawn_session_end_ingest(
    store: Arc<Mutex<InterestStore>>,
    config: InterestConfig,
    messages: Vec<Value>,
    buffered: Vec<InterestSignal>,
    auxiliary: Option<Arc<AuxiliaryClient>>,
) {
    if !config.enabled {
        return;
    }
    if tokio::runtime::Handle::try_current().is_err() {
        tracing::debug!("interest: skip session-end ingest without tokio runtime");
        return;
    }
    tokio::spawn(async move {
        let mut all_signals = buffered;
        if config.session_end_llm_enabled() {
            if let Some(aux) = auxiliary.as_ref() {
                let transcript = format_user_transcript(&messages);
                let existing_labels = store
                    .lock()
                    .ok()
                    .and_then(|g| g.top_labels_for_llm(5).ok())
                    .unwrap_or_default();
                all_signals.extend(
                    extract_signals_from_transcript_llm(aux, &transcript, &existing_labels).await,
                );
            } else {
                tracing::debug!(
                    "interest: session_end_llm_enabled but no auxiliary client; skipping LLM extract"
                );
            }
        }
        if config.uses_rules() {
            all_signals.extend(extract_signals_from_messages(&messages));
        }
        let all_signals = filter_persistable_signals(filter_poi_signals(all_signals));
        if all_signals.is_empty() {
            return;
        }
        if let Ok(guard) = store.lock() {
            let _ = guard.apply_decay();
            match apply_signal_batch(&guard, &config, all_signals) {
                Ok(report) => {
                    if report.inserted + report.reinforced + report.merged > 0 {
                        info!(
                            inserted = report.inserted,
                            reinforced = report.reinforced,
                            merged = report.merged,
                            promoted = report.promoted,
                            skipped = report.skipped,
                            "interest: session-end POI pipeline applied"
                        );
                    }
                }
                Err(err) => tracing::warn!("interest: session-end pipeline failed: {err}"),
            }
        }
    });
}

/// Concatenate user-role message text only (privacy: no assistant/tool content to LLM).
pub fn format_user_transcript_for_llm(messages: &[Value]) -> String {
    format_user_transcript(messages)
}

/// Concatenate user-role message text only (privacy: no assistant/tool content to LLM).
fn format_user_transcript(messages: &[Value]) -> String {
    let mut out = String::new();
    for msg in messages {
        let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
        if !role.eq_ignore_ascii_case("user") {
            continue;
        }
        let content = message_text_from_value(msg);
        let trimmed = content.trim();
        if trimmed.is_empty() || is_poi_synthetic_user_text(trimmed) {
            continue;
        }
        out.push_str(trimmed);
        out.push_str("\n\n");
    }
    out
}
