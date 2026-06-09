//! Resolution Verdict Engine — local POI solve-quality labeling.

use hermes_insights::types::ResolutionPayload;

use crate::user_interest::{is_poi_synthetic_user_text, message_text_from_value};

#[derive(Debug, Clone)]
pub struct SessionSignals {
    pub user_turns: u32,
    pub tool_failures: u32,
    pub tool_successes: u32,
    pub skill_patched: bool,
    pub skill_created: bool,
    pub explicit_positive: bool,
    pub explicit_negative: bool,
    pub correction_loops: u32,
    pub closure_without_followup: bool,
}

pub fn analyze_session(
    messages: &[serde_json::Value],
    skill_summary: &hermes_insights::SessionSkillSummary,
) -> SessionSignals {
    let mut user_turns = 0u32;
    let mut user_messages: Vec<String> = Vec::new();
    let mut tool_failures = 0u32;
    let mut tool_successes = 0u32;

    for msg in messages {
        let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
        if role.eq_ignore_ascii_case("user") {
            let text = message_text_from_value(msg);
            let trimmed = text.trim();
            if trimmed.is_empty() || is_poi_synthetic_user_text(trimmed) {
                continue;
            }
            user_turns += 1;
            user_messages.push(trimmed.to_string());
        } else if role.eq_ignore_ascii_case("tool") {
            let content = message_text_from_value(msg).to_ascii_lowercase();
            if content.contains("\"success\": false")
                || content.contains("error")
                || content.contains("failed")
            {
                tool_failures += 1;
            } else if content.contains("\"success\": true") || content.contains("success") {
                tool_successes += 1;
            }
        }
    }

    let explicit_positive = user_messages.iter().any(|m| matches_feedback(m, POSITIVE_PATTERNS));
    let explicit_negative = user_messages.iter().any(|m| matches_feedback(m, NEGATIVE_PATTERNS));
    let correction_loops = count_correction_loops(&user_messages);
    let closure_without_followup = user_turns >= 2
        && !explicit_negative
        && user_messages
            .last()
            .is_some_and(|m| m.chars().count() < 120);

    SessionSignals {
        user_turns,
        tool_failures,
        tool_successes,
        skill_patched: skill_summary.patch_count > 0,
        skill_created: skill_summary.skill_created,
        explicit_positive,
        explicit_negative,
        correction_loops,
        closure_without_followup,
    }
}

pub fn fuse_verdict(signals: &SessionSignals) -> ResolutionPayload {
    let mut codes = Vec::new();

    if signals.user_turns < 2 {
        codes.push("insufficient_turns".to_string());
        return resolution_payload(
            "abandoned",
            "low",
            "D",
            "unknown",
            None,
            codes,
            false,
        );
    }

    if signals.explicit_negative {
        codes.push("user_explicit_negative".to_string());
        if signals.correction_loops > 0 {
            codes.push("user_correction_loop".to_string());
        }
        let objective = objective_band(signals);
        return resolution_payload(
            "failed",
            "high",
            if signals.tool_failures > 0 { "B" } else { "A" },
            "explicit_negative",
            objective,
            codes,
            signals.skill_patched,
        );
    }

    if signals.explicit_positive {
        codes.push("user_explicit_positive".to_string());
        if signals.closure_without_followup {
            codes.push("closure_without_followup".to_string());
        }
        let objective = objective_band(signals);
        if objective.as_deref() == Some("pass") {
            codes.push("objective_test_pass".to_string());
        }
        if signals.skill_patched {
            codes.push("skill_patched_this_session".to_string());
        }
        if signals.skill_created {
            codes.push("skill_created_this_session".to_string());
        }
        return resolution_payload(
            "solved_confirmed",
            "high",
            "A",
            "explicit_positive",
            objective,
            codes,
            false,
        );
    }

    if signals.tool_failures > 0 && signals.tool_successes == 0 {
        codes.push("objective_test_fail".to_string());
        return resolution_payload(
            "partial",
            "medium",
            "C",
            "neutral",
            Some("fail".to_string()),
            codes,
            signals.skill_patched,
        );
    }

    if signals.tool_successes > 0 && signals.correction_loops == 0 {
        codes.push("objective_test_pass".to_string());
        if signals.closure_without_followup {
            codes.push("closure_without_followup".to_string());
        }
        if signals.skill_patched {
            codes.push("skill_patched_this_session".to_string());
        }
        return resolution_payload(
            "solved_inferred",
            "medium",
            "B",
            "neutral",
            Some("pass".to_string()),
            codes,
            signals.skill_patched,
        );
    }

    if signals.correction_loops > 0 {
        codes.push("user_correction_loop".to_string());
        return resolution_payload(
            "partial",
            "medium",
            "C",
            "neutral",
            objective_band(signals),
            codes,
            signals.skill_patched,
        );
    }

    codes.push("closure_without_followup".to_string());
    resolution_payload(
        "unresolved",
        "low",
        "D",
        "unknown",
        Some("not_applicable".to_string()),
        codes,
        false,
    )
}

fn resolution_payload(
    verdict: &str,
    confidence_band: &str,
    evidence_tier: &str,
    user_feedback_band: &str,
    objective_check_band: Option<String>,
    signal_codes: Vec<String>,
    recovery_attempted: bool,
) -> ResolutionPayload {
    ResolutionPayload {
        verdict: verdict.to_string(),
        confidence_band: confidence_band.to_string(),
        evidence_tier: evidence_tier.to_string(),
        user_feedback_band: user_feedback_band.to_string(),
        objective_check_band,
        signal_codes,
        recovery_attempted,
    }
}

fn objective_band(signals: &SessionSignals) -> Option<String> {
    if signals.tool_successes == 0 && signals.tool_failures == 0 {
        return Some("not_applicable".to_string());
    }
    if signals.tool_failures > signals.tool_successes {
        Some("fail".to_string())
    } else {
        Some("pass".to_string())
    }
}

fn matches_feedback(text: &str, patterns: &[&str]) -> bool {
    let lower = text.to_ascii_lowercase();
    patterns.iter().any(|p| lower.contains(p))
}

fn count_correction_loops(messages: &[String]) -> u32 {
    messages
        .iter()
        .filter(|m| matches_feedback(m, CORRECTION_PATTERNS))
        .count() as u32
}

const POSITIVE_PATTERNS: &[&str] = &[
    "解决了", "可以了", "好的", "谢谢", "感谢", "perfect", "works now", "that worked",
    "looks good", "great job", "ok thanks", "没问题",
];

const NEGATIVE_PATTERNS: &[&str] = &[
    "不对", "不行", "还是错", "没用", "不要这样", "wrong", "not working", "doesn't work",
    "still broken", "try again", "incorrect",
];

const CORRECTION_PATTERNS: &[&str] = &[
    "不对", "错了", "应该是", "别这样", "instead", "don't do", "stop doing", "not like that",
];

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_insights::SessionSkillSummary;

    #[test]
    fn explicit_positive_yields_solved_confirmed() {
        let messages = vec![
            serde_json::json!({"role":"user","content":"help with ledger reconciliation"}),
            serde_json::json!({"role":"user","content":"可以了，谢谢"}),
        ];
        let signals = analyze_session(&messages, &SessionSkillSummary::default());
        let resolution = fuse_verdict(&signals);
        assert_eq!(resolution.verdict, "solved_confirmed");
        assert_eq!(resolution.evidence_tier, "A");
    }
}
