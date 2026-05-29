//! Optional LLM-based topic extraction via the auxiliary client (credential pool + fallbacks).

use std::time::Duration;

use hermes_core::Message;
use hermes_intelligence::auxiliary::{AuxiliaryClient, AuxiliaryRequest, AuxiliaryTask};

use super::extract::parse_llm_topics_json;
use super::store::InterestSignal;

const INTEREST_LLM_TASK: &str = "interest";
const MAX_USER_TRANSCRIPT_CHARS: usize = 12_000;

/// Extract interest topics from **user-only** transcript text via auxiliary LLM routing.
pub async fn extract_signals_from_transcript_llm(
    auxiliary: &AuxiliaryClient,
    user_transcript: &str,
) -> Vec<InterestSignal> {
    let trimmed = user_transcript.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let body = if trimmed.chars().count() > MAX_USER_TRANSCRIPT_CHARS {
        format!(
            "{}\n…[truncated]",
            trimmed.chars().take(MAX_USER_TRANSCRIPT_CHARS).collect::<String>()
        )
    } else {
        trimmed.to_string()
    };

    let system = "You extract durable user interest topics from agent conversations. \
                  Output ONLY a JSON array (no markdown). Each item: \
                  {\"label\": string, \"summary\": string, \"confidence\": 0-1, \"tags\": [string]}. \
                  Max 3 items. Focus on recurring goals, tech stacks, projects — not one-off chit-chat.";
    let user = format!(
        "Extract up to 3 user interest topics from these user messages only:\n\n{body}"
    );

    let request = AuxiliaryRequest::new(
        AuxiliaryTask::Custom(INTEREST_LLM_TASK.to_string()),
        vec![Message::system(system), Message::user(user)],
    )
    .with_temperature(0.1)
    .with_max_tokens(800)
    .with_timeout(Duration::from_secs(60));

    match auxiliary.call(request).await {
        Ok(resp) => {
            let text = resp.text().unwrap_or_default();
            parse_llm_topics_json(text)
        }
        Err(err) => {
            tracing::debug!("interest LLM extraction skipped: {err}");
            Vec::new()
        }
    }
}
