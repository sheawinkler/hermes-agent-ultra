//! Extract domain POI candidates from session user text.

use hermes_insights::sanitize::{sanitize_text, slugify_name};
use hermes_insights::types::DomainPoiPayload;

use crate::user_interest::{
    extract_contextual_interests, extract_declared_interests, extract_signals_from_messages,
    is_poi_synthetic_user_text, message_text_from_value, InterestSignal, SignalSource,
};

#[derive(Debug, Clone)]
pub struct DomainCandidate {
    pub domain_key: String,
    pub problem_statement_redacted: String,
    pub problem_class: String,
    pub difficulty_band: String,
    pub taxonomy_code: Option<String>,
    pub confidence: f64,
}

pub fn extract_domain_candidate(messages: &[serde_json::Value]) -> Option<DomainCandidate> {
    let user_text = user_transcript(messages);
    if user_text.trim().len() < 12 {
        return None;
    }

    let mut signals: Vec<InterestSignal> = Vec::new();
    signals.extend(extract_declared_interests(&user_text, 1.0));
    signals.extend(extract_contextual_interests(&user_text, 1.0));
    signals.extend(extract_signals_from_messages(messages));

    let best = signals
        .into_iter()
        .filter(|s| !is_noise_signal(s))
        .max_by(|a, b| {
            a.confidence
                .partial_cmp(&b.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
        })?;

    let label = sanitize_text(&best.label);
    let summary = sanitize_text(if best.summary.is_empty() {
        &label
    } else {
        &best.summary
    });
    if label.is_empty() || summary.is_empty() {
        return None;
    }

    let domain_key = domain_key_from_signal(&best, &label);
    let problem_class = infer_problem_class(messages, &best);
    let difficulty = if user_text.chars().count() > 400 {
        "high"
    } else if user_text.chars().count() > 120 {
        "med"
    } else {
        "low"
    };

    Some(DomainCandidate {
        domain_key,
        problem_statement_redacted: summary,
        problem_class: problem_class.to_string(),
        difficulty_band: difficulty.to_string(),
        taxonomy_code: taxonomy_hint(&best),
        confidence: best.confidence,
    })
}

pub fn candidate_to_poi(candidate: &DomainCandidate) -> DomainPoiPayload {
    DomainPoiPayload {
        domain_key: candidate.domain_key.clone(),
        taxonomy_code: candidate.taxonomy_code.clone(),
        problem_class: candidate.problem_class.clone(),
        problem_statement_redacted: candidate.problem_statement_redacted.clone(),
        difficulty_band: candidate.difficulty_band.clone(),
    }
}

fn user_transcript(messages: &[serde_json::Value]) -> String {
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
        out.push('\n');
    }
    out
}

fn is_noise_signal(signal: &InterestSignal) -> bool {
    let id = signal.id.to_ascii_lowercase();
    id.starts_with("path:") || id.starts_with("keyword:") || signal.label.trim().is_empty()
}

fn domain_key_from_signal(signal: &InterestSignal, label: &str) -> String {
    let id = signal.id.trim().to_ascii_lowercase();
    if id.starts_with("lang:") || id.starts_with("tech:") || id.starts_with("topic:") {
        return id;
    }
    let slug = slugify_name(label);
    if slug.is_empty() {
        format!(
            "topic:domain-{}",
            &hermes_insights::types::sha256_hex(label.as_bytes())[..8]
        )
    } else {
        format!("topic:{slug}")
    }
}

fn infer_problem_class(messages: &[serde_json::Value], signal: &InterestSignal) -> &'static str {
    if matches!(signal.source, SignalSource::Lang | SignalSource::Tech) {
        return "technical";
    }
    if transcript_mentions_tool(messages, "execute_code") {
        return "technical";
    }
    if signal.tags.iter().any(|t| t.contains("research")) {
        return "research";
    }
    if signal.tags.iter().any(|t| t.contains("creative")) {
        return "creative";
    }
    "operational"
}

fn taxonomy_hint(signal: &InterestSignal) -> Option<String> {
    if matches!(signal.source, SignalSource::Lang) {
        let lang = signal.id.strip_prefix("lang:")?;
        return Some(format!("software.lang.{lang}"));
    }
    if matches!(signal.source, SignalSource::Tech) {
        let tech = signal.id.strip_prefix("tech:")?;
        return Some(format!("software.tech.{tech}"));
    }
    None
}

fn transcript_mentions_tool(messages: &[serde_json::Value], tool_name: &str) -> bool {
    messages.iter().any(|msg| {
        msg.get("role")
            .and_then(|v| v.as_str())
            .is_some_and(|r| r == "assistant")
            && msg
                .get("tool_calls")
                .and_then(|v| v.as_array())
                .is_some_and(|arr| {
                    arr.iter().any(|tc| {
                        tc.get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|n| n.as_str())
                            == Some(tool_name)
                    })
                })
    })
}
