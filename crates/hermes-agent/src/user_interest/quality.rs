//! Turn gating and persistence quality (shared policy with `hermes-insights`).

use hermes_insights::sanitize::is_persistable_local_poi;

use super::declared::extract_declared_interests;
use super::ingest::is_poi_synthetic_user_text;
use super::task_oriented::has_task_or_domain_signal;
use super::store::InterestSignal;
use super::types::SignalSource;

/// Whether this user turn should run any extraction (buffer or persist).
pub fn should_extract_user_turn(text: &str, min_turn_chars: u32) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() || is_poi_synthetic_user_text(trimmed) {
        return false;
    }
    if !extract_declared_interests(trimmed, 1.0).is_empty() {
        return true;
    }
    if has_task_or_domain_signal(trimmed) {
        return true;
    }
    if trimmed.chars().count() < min_turn_chars as usize {
        return false;
    }
    if looks_like_meta_command(trimmed) {
        return false;
    }
    true
}

fn looks_like_meta_command(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let trimmed = lower.trim();
    trimmed.starts_with("hermes interest")
        || trimmed.starts_with("hermes contribute")
        || trimmed.starts_with("/interest")
        || trimmed.starts_with("/reset")
        || trimmed.starts_with("/new")
        || trimmed.starts_with("/help")
        || trimmed.starts_with("/start")
        || trimmed.starts_with("/clear")
        || trimmed == "ok"
        || trimmed == "okay"
        || trimmed == "thanks"
        || trimmed == "thank you"
        || trimmed == "好的"
        || trimmed == "谢谢"
        || trimmed == "收到"
        || trimmed == "明白了"
        || trimmed == "知道了"
        || is_ack_only(trimmed)
}

fn is_ack_only(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() {
        return true;
    }
    // Pure punctuation / emoji-ish short replies
    if t.chars().count() <= 3 && t.chars().all(|c| !c.is_alphanumeric() && !c.is_alphabetic()) {
        return true;
    }
    matches!(
        t,
        "嗯"
            | "嗯嗯"
            | "行"
            | "可以"
            | "没问题"
            | "go ahead"
            | "sure"
            | "yes"
            | "yep"
            | "👍"
            | "👌"
    )
}

/// Filter signals before compare/update (blocklist + insights-aligned persist gate).
pub fn filter_persistable_signals(signals: Vec<InterestSignal>) -> Vec<InterestSignal> {
    signals
        .into_iter()
        .filter(|s| is_persistable_signal(s))
        .collect()
}

pub fn is_persistable_signal(signal: &InterestSignal) -> bool {
    if !is_persistable_local_poi(&signal.id, &signal.label) {
        return false;
    }
    let source = signal.source();
    if matches!(source, SignalSource::Keyword | SignalSource::Path) {
        return false;
    }
    signal.confidence > 0.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_interest::extract::extract_signals_from_text;
    use crate::user_interest::types::ExtractOptions;

    #[test]
    fn short_chitchat_skipped() {
        assert!(!should_extract_user_turn("ok", 12));
        assert!(!should_extract_user_turn("thanks", 12));
    }

    #[test]
    fn declared_short_text_allowed() {
        assert!(should_extract_user_turn("我的兴趣点是打篮球", 12));
    }

    #[test]
    fn task_request_short_turn_allowed() {
        assert!(should_extract_user_turn(
            "帮我看看当前时间上A股行情怎么样了？",
            12
        ));
        assert!(should_extract_user_turn(
            "这30万是我的全部积蓄；投资期5年；要稳健点",
            12
        ));
    }

    #[test]
    fn reset_command_skipped() {
        assert!(!should_extract_user_turn("/reset", 12));
        assert!(!should_extract_user_turn("/new", 12));
    }

    #[test]
    fn keyword_signals_not_persistable() {
        let signals = extract_signals_from_text(
            "implement database migration for hermes interest store",
            1.0,
            ExtractOptions {
                include_keywords: true,
            },
        );
        let persistable: Vec<_> = filter_persistable_signals(signals);
        assert!(persistable.iter().all(|s| !s.id.starts_with("keyword:")));
    }
}
