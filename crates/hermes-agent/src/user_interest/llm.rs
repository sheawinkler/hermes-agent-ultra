//! LLM-based POI extraction and summarization via the auxiliary client.

use std::time::Duration;

use hermes_core::Message;
use hermes_intelligence::auxiliary::{AuxiliaryClient, AuxiliaryRequest, AuxiliaryTask};
use tracing::debug;

use super::domain_taxonomy::domain_taxonomy_prompt_block;
use super::extract::parse_llm_topics_json;
use super::store::InterestSignal;

const INTEREST_LLM_TASK: &str = "interest";
const MAX_USER_TRANSCRIPT_CHARS: usize = 12_000;

fn interest_llm_system_prompt() -> String {
    format!(
        r#"You extract durable user interest topics (POI) from real conversational utterances.

Users rarely say "my interest is X". They ask for help, state constraints, or describe situations.
Your job is semantic inference — do NOT rely on keyword lists or fixed categories.

Output ONLY a JSON array (no markdown fences). Each item:
{{"label": string, "summary": string, "confidence": 0-1, "tags": [string], "domain_key": string|null}}

Field rules:
- label: short human-readable topic (Chinese or English matching the user; ≤ 40 chars).
- summary: 1-3 sentences describing the user's ongoing concern or goal. Generalize and redact PII
  (no exact amounts, account numbers, names, addresses, employer names).
- confidence: 0-1 how durable/recurring this interest is (not how confident you are linguistically).
- tags: optional facets, e.g. "finance", "constraint", "career", "task".
- domain_key: best-matching taxonomy key when one fits, else null or a new snake_case key.

Quality rules:
- Max 4 items. Prefer 1-2 high-quality topics over noisy lists.
- Infer from meaning across the whole transcript — tasks, constraints, repeated themes, decisions.
- Skip pure chit-chat ("thanks", "ok", "hello") with no durable domain signal.
- One-off ephemeral lookups (e.g. today's weather) → omit unless they reveal a recurring domain.
- Merge near-duplicates; refine existing topics when listed below instead of cloning.
- For engineering sessions, prefer durable stack/domain over one-off file paths.

{taxonomy}"#,
        taxonomy = domain_taxonomy_prompt_block()
    )
}

/// Extract interest topics from **user-only** transcript text via auxiliary LLM routing.
pub async fn extract_signals_from_transcript_llm(
    auxiliary: &AuxiliaryClient,
    user_transcript: &str,
    existing_topic_labels: &[String],
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

    let existing_block = if existing_topic_labels.is_empty() {
        String::new()
    } else {
        format!(
            "\n\nExisting user topics (merge or refine; do not duplicate):\n- {}\n",
            existing_topic_labels.join("\n- ")
        )
    };

    let user = format!(
        "Extract durable user interest topics from these user messages only.{existing_block}\n\n{body}"
    );

    let request = AuxiliaryRequest::new(
        AuxiliaryTask::Custom(INTEREST_LLM_TASK.to_string()),
        vec![
            Message::system(interest_llm_system_prompt()),
            Message::user(user),
        ],
    )
    .with_temperature(0.15)
    .with_max_tokens(1200)
    .with_timeout(Duration::from_secs(90));

    match auxiliary.call(request).await {
        Ok(resp) => {
            let text = resp.text().unwrap_or_default();
            let parsed = parse_llm_topics_json(text);
            if parsed.is_empty() && !text.trim().is_empty() {
                debug!(
                    chars = text.chars().count(),
                    "interest LLM returned no parseable topics"
                );
            }
            parsed
        }
        Err(err) => {
            tracing::warn!("interest LLM extraction failed: {err}");
            Vec::new()
        }
    }
}
