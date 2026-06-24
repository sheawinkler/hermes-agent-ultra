//! Exact-match sleep / mute phrases that skip LLM and enter dormant mode.

/// Strip whitespace, line breaks, and punctuation before exact keyword comparison.
fn normalize_sleep_phrase(s: &str) -> String {
    let stripped: String = s
        .chars()
        .filter(|c| !is_ignorable_for_sleep_match(*c))
        .collect();
    if stripped.is_ascii() {
        stripped.to_ascii_lowercase()
    } else {
        stripped
    }
}

fn is_ignorable_for_sleep_match(c: char) -> bool {
    c.is_whitespace()
        || c.is_ascii_punctuation()
        || matches!(
            c,
            '，' | '。'
                | '！'
                | '？'
                | '…'
                | '；'
                | '：'
                | '、'
                | '．'
                | '「'
                | '」'
                | '『'
                | '』'
                | '（'
                | '）'
                | '【'
                | '】'
                | '《'
                | '》'
                | '—'
                | '–'
                | '·'
                | '〜'
                | '～'
                | '﹑'
                | '﹒'
                | '﹖'
                | '﹗'
        )
}

/// Return true when `text` exactly matches one of `phrases` (after normalization).
///
/// ASR trailing punctuation, spaces, and newlines are removed before compare.
/// ASCII phrases compare case-insensitively; CJK phrases compare literally.
pub fn matches_sleep_keyword(text: &str, phrases: &[String]) -> bool {
    let normalized = normalize_sleep_phrase(text);
    if normalized.is_empty() {
        return false;
    }
    phrases.iter().any(|phrase| {
        let candidate = normalize_sleep_phrase(phrase);
        !candidate.is_empty() && normalized == candidate
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn phrases() -> Vec<String> {
        vec![
            "休眠".into(),
            "安静".into(),
            "mute".into(),
            "闭嘴".into(),
            "shut up".into(),
        ]
    }

    #[test]
    fn exact_match_hits() {
        assert!(matches_sleep_keyword("安静", &phrases()));
        assert!(matches_sleep_keyword("休眠", &phrases()));
        assert!(matches_sleep_keyword("mute", &phrases()));
        assert!(matches_sleep_keyword("Mute", &phrases()));
    }

    #[test]
    fn partial_or_prefix_does_not_hit() {
        assert!(!matches_sleep_keyword("请安静", &phrases()));
        assert!(!matches_sleep_keyword("安静点", &phrases()));
        assert!(!matches_sleep_keyword("进入休眠", &phrases()));
        assert!(!matches_sleep_keyword("muted", &phrases()));
    }

    #[test]
    fn strips_whitespace_punctuation_and_newlines() {
        assert!(matches_sleep_keyword("  安静  ", &phrases()));
        assert!(matches_sleep_keyword("安静。", &phrases()));
        assert!(matches_sleep_keyword("安静，\r\n", &phrases()));
        assert!(matches_sleep_keyword("mute!", &phrases()));
        assert!(matches_sleep_keyword(" mute ，", &phrases()));
        assert!(matches_sleep_keyword("shut up。", &phrases()));
        assert!(matches_sleep_keyword(" shut\nup \t", &phrases()));
    }

    #[test]
    fn empty_phrase_list_never_matches() {
        assert!(!matches_sleep_keyword("安静", &[]));
    }
}
