/// First speakable chunk for low-latency TTS (before full sentence).
pub fn take_early_chunk(buf: &mut String, min_chars: usize) -> Option<String> {
    let count = buf.chars().count();
    if count < min_chars {
        return None;
    }
    let s: String = buf.chars().take(min_chars).collect();
    let rest: String = buf.chars().skip(min_chars).collect();
    if s.trim().is_empty() {
        return None;
    }
    *buf = rest;
    Some(s)
}

/// Extract a speakable sentence from the LLM buffer if ready.
pub fn take_sentence(buf: &mut String, min_len: usize) -> Option<String> {
    let delimiters = ['。', '！', '？', '\n', '.', '!', '?'];
    let split_at = buf
        .char_indices()
        .find_map(|(i, ch)| delimiters.contains(&ch).then_some(i + ch.len_utf8()));
    if let Some(end) = split_at {
        let sentence: String = buf.drain(..end).collect();
        let trimmed = sentence.trim().to_string();
        if !trimmed.is_empty() {
            return Some(trimmed);
        }
    }
    if buf.chars().count() >= min_len {
        let s = buf.trim().to_string();
        if !s.is_empty() {
            buf.clear();
            return Some(s);
        }
    }
    None
}

pub fn flush_remainder(buf: &mut String) -> Option<String> {
    let s = buf.trim().to_string();
    buf.clear();
    if s.is_empty() { None } else { Some(s) }
}

/// Replace typographic quotes and convert Arabic numerals to Chinese.
pub fn normalize_tts_text(text: &str) -> String {
    let text = super::normalizer::normalize_chinese_numbers(text);
    super::normalizer::normalize_quotes(&text)
}

/// Whether ASR final is compatible with an earlier speculative partial.
pub fn texts_compatible(partial: &str, final_text: &str) -> bool {
    fn norm(s: &str) -> String {
        s.chars()
            .filter(|c| {
                !c.is_whitespace() && !['，', '。', '？', '！', '.', ',', '?', '!'].contains(c)
            })
            .collect()
    }
    let a = norm(partial);
    let b = norm(final_text);
    if a.is_empty() || b.is_empty() {
        return false;
    }
    a == b || a.starts_with(&b) || b.starts_with(&a)
}
