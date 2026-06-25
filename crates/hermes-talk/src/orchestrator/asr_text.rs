//! Pick the best ASR transcript when streaming engines emit early finals and later partials.

/// Merge two fragments when the suffix of `a` overlaps the prefix of `b`.
fn merge_asr_with_overlap(a: &str, b: &str) -> Option<String> {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let max_overlap = a_chars.len().min(b_chars.len());
    for overlap in (1..=max_overlap).rev() {
        if a_chars[a_chars.len() - overlap..] == b_chars[..overlap] {
            let mut merged = String::with_capacity(a.len() + b.len());
            merged.extend(a_chars.iter());
            merged.extend(b_chars[overlap..].iter());
            return Some(merged);
        }
    }
    None
}

/// Track the longest / most complete ASR hypothesis seen during an utterance.
pub fn update_best_asr_text(best: &mut String, text: &str) {
    let t = text.trim();
    if t.is_empty() {
        return;
    }
    if best.is_empty() {
        *best = t.to_string();
        return;
    }
    if let Some(merged) = pick_best_asr_transcript(&[best.as_str(), t]) {
        *best = merged;
    }
}

/// Choose the best transcript from finals, partials, and accumulated candidates.
pub fn pick_best_asr_transcript(candidates: &[&str]) -> Option<String> {
    let trimmed: Vec<String> = candidates
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    if trimmed.is_empty() {
        return None;
    }

    let mut best = trimmed[0].clone();
    for t in trimmed.iter().skip(1) {
        if let Some(merged) = merge_asr_with_overlap(&best, t) {
            if merged.chars().count() > best.chars().count() {
                best = merged;
                continue;
            }
        }
        if let Some(merged) = merge_asr_with_overlap(t, &best) {
            if merged.chars().count() > best.chars().count() {
                best = merged;
                continue;
            }
        }
        if t.chars().count() > best.chars().count() {
            best.clone_from(t);
        }
    }
    Some(best)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_overlap_fragments() {
        let best = pick_best_asr_transcript(&["帮我查", "查一下明天的天气。"]).unwrap();
        assert_eq!(best, "帮我查一下明天的天气。");
    }

    #[test]
    fn prefers_longer_non_overlapping_partial() {
        let best = pick_best_asr_transcript(&["帮我查", "一下明天的天气"]).unwrap();
        assert_eq!(best.chars().count(), "一下明天的天气".chars().count());
    }

    #[test]
    fn update_best_keeps_longest() {
        let mut best = String::new();
        update_best_asr_text(&mut best, "帮我查");
        update_best_asr_text(&mut best, "查一下明天的天气。");
        assert_eq!(best, "帮我查一下明天的天气。");
    }
}
