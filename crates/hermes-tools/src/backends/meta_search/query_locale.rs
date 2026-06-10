//! Detect whether a query should trigger CN meta-search engines.

/// Returns true when the query contains CJK unified ideographs (common Chinese/Japanese/Korean text).
pub fn query_has_cjk(query: &str) -> bool {
    query.chars().any(|ch| {
        matches!(
            ch,
            '\u{3400}'..='\u{4DBF}'   // CJK Extension A
                | '\u{4E00}'..='\u{9FFF}' // CJK Unified
                | '\u{F900}'..='\u{FAFF}' // CJK Compatibility
                | '\u{20000}'..='\u{2A6DF}' // Extension B (needs surrogate pairs in UTF-16; char covers it)
                | '\u{2A700}'..='\u{2B73F}'
                | '\u{2B740}'..='\u{2B81F}'
                | '\u{2B820}'..='\u{2CEAF}'
                | '\u{2CEB0}'..='\u{2EBEF}'
                | '\u{30000}'..='\u{3134F}'
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latin_only_is_false() {
        assert!(!query_has_cjk("hello world"));
        assert!(!query_has_cjk("Rust programming"));
    }

    #[test]
    fn chinese_is_true() {
        assert!(query_has_cjk("Rust 编程"));
        assert!(query_has_cjk("人工智能"));
    }

    #[test]
    fn mixed_and_emoji() {
        assert!(query_has_cjk("AI 新闻 2026"));
        assert!(!query_has_cjk("🚀 rocket"));
        assert!(!query_has_cjk(""));
    }
}
