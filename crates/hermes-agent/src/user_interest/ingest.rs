//! Shared POI ingestion helpers (agent loop + memory plugin).

use std::sync::{Arc, Mutex};

use hermes_config::InterestConfig;
use hermes_intelligence::auxiliary::AuxiliaryClient;
use serde_json::Value;

use super::extract::{
    extract_signals_from_messages, extract_signals_from_text, filter_poi_signals,
    message_text_from_value,
};
use super::llm::extract_signals_from_transcript_llm;
use super::store::InterestStore;

/// Agent-injected continuation / nudge user lines — not real user POI.
pub fn is_poi_synthetic_user_text(text: &str) -> bool {
    let t = text.trim();
    t.starts_with("[System:")
        && (t.contains("Continue now")
            || t.contains("incomplete due to generation limits")
            || t.contains("Continue exactly where you left off"))
}

/// Rule-based ingest from a single user message (per-turn).
pub fn ingest_user_message(
    store: &InterestStore,
    user_text: &str,
    weight_scale: f64,
) -> Result<(), String> {
    let trimmed = user_text.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    let signals = filter_poi_signals(extract_signals_from_text(trimmed, weight_scale));
    if signals.is_empty() {
        return Ok(());
    }
    store.ingest_signals(&signals)
}

/// Session-end ingest: user-only rules + optional async LLM summary (auxiliary client).
pub fn spawn_session_end_ingest(
    store: Arc<Mutex<InterestStore>>,
    config: InterestConfig,
    messages: Vec<Value>,
    auxiliary: Option<Arc<AuxiliaryClient>>,
) {
    if !config.enabled {
        return;
    }
    tokio::spawn(async move {
        let mut all_signals = Vec::new();
        if config.uses_rules() {
            all_signals.extend(extract_signals_from_messages(&messages));
        }
        if config.session_end_llm_enabled() {
            if let Some(aux) = auxiliary.as_ref() {
                let transcript = format_user_transcript(&messages);
                all_signals.extend(
                    extract_signals_from_transcript_llm(aux, &transcript).await,
                );
            } else {
                tracing::debug!(
                    "interest: session_end_llm_enabled but no auxiliary client; skipping LLM extract"
                );
            }
        }
        let all_signals = filter_poi_signals(all_signals);
        if all_signals.is_empty() {
            return;
        }
        if let Ok(guard) = store.lock() {
            let _ = guard.apply_decay();
            let _ = guard.ingest_signals(&all_signals);
        }
    });
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
