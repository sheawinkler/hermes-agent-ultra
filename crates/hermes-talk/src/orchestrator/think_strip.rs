//! Strip `<think...>...</think>` blocks from streaming LLM text before TTS.
//!
//! Ported from `hermes-tools::tts_streaming::sanitizer::IncrementalThinkStripper` so talk
//! does not depend on the full tools crate.

use std::sync::OnceLock;

use regex::Regex;

const CLOSE_TAGS: &[&str] = &[
    "</think>",
    "</thinking>",
    "</thought>",
    "</reasoning>",
    "</REASONING_SCRATCHPAD>",
];

fn closed_redacted_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?is)<think>.*?</think>").unwrap())
}

fn closed_thinking_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?is)<thinking>.*?</thinking>").unwrap())
}

fn closed_thought_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?is)<thought>.*?</thought>").unwrap())
}

fn closed_reason_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?is)<reasoning>.*?</reasoning>").unwrap())
}

fn closed_scratch_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?s)<REASONING_SCRATCHPAD>.*?</REASONING_SCRATCHPAD>").unwrap())
}

fn unterminated_think_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?is)(?:^|\n)[ \t]*<(?:redacted_thinking|think|thinking|reasoning|thought|REASONING_SCRATCHPAD)\b[^>]*>.*$",
        )
        .unwrap()
    })
}

fn orphan_think_tags_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)</?(?:redacted_thinking|think|thinking|reasoning|thought|REASONING_SCRATCHPAD)>\s*",
        )
        .unwrap()
    })
}

fn extract_patterns() -> &'static [Regex; 5] {
    static PATTERNS: OnceLock<[Regex; 5]> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        [
            Regex::new(r"(?is)<think>(.*?)</think>").unwrap(),
            Regex::new(r"(?is)<thinking>(.*?)</thinking>").unwrap(),
            Regex::new(r"(?is)<thought>(.*?)</thought>").unwrap(),
            Regex::new(r"(?is)<reasoning>(.*?)</reasoning>").unwrap(),
            Regex::new(r"(?s)<REASONING_SCRATCHPAD>(.*?)</REASONING_SCRATCHPAD>").unwrap(),
        ]
    })
}

/// Remove thinking / reasoning XML blocks from a complete assistant string.
pub fn strip_think_blocks(content: &str) -> String {
    if content.is_empty() {
        return String::new();
    }
    let mut c = content.to_string();
    c = closed_redacted_re().replace_all(&c, "").to_string();
    c = closed_thinking_re().replace_all(&c, "").to_string();
    c = closed_reason_re().replace_all(&c, "").to_string();
    c = closed_scratch_re().replace_all(&c, "").to_string();
    c = closed_thought_re().replace_all(&c, "").to_string();
    c = unterminated_think_re().replace_all(&c, "").to_string();
    c = orphan_think_tags_re().replace_all(&c, "").to_string();
    c.trim().to_string()
}

/// Collect inner text from closed thinking blocks (for reasoning logs when the model
/// embeds CoT in `content` instead of `reasoning_content`).
pub fn extract_inline_thinking(content: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    for re in extract_patterns() {
        for cap in re.captures_iter(content) {
            if let Some(m) = cap.get(1) {
                let s = m.as_str().trim();
                if !s.is_empty() {
                    parts.push(s.to_string());
                }
            }
        }
    }
    // Unterminated opening tag at end of stream (common with local rkllm).
    if let Some(caps) = Regex::new(
        r"(?is)<(?:redacted_thinking|think|thinking|reasoning|thought|REASONING_SCRATCHPAD)\b[^>]*>(.*)$",
    )
    .unwrap()
    .captures(content)
    {
        if let Some(m) = caps.get(1) {
            let s = m.as_str().trim();
            if !s.is_empty() && !parts.iter().any(|p| p.contains(s) || s.contains(p)) {
                parts.push(s.to_string());
            }
        }
    }
    parts.join("\n")
}

fn find_close_tag(buf: &str) -> Option<(usize, &'static str)> {
    CLOSE_TAGS
        .iter()
        .filter_map(|tag| buf.find(tag).map(|pos| (pos, *tag)))
        .min_by_key(|(pos, _)| *pos)
}

/// Stateful filter that removes model thinking blocks from a streaming text source.
#[derive(Debug, Default)]
pub struct IncrementalThinkStripper {
    pending: String,
    inside: bool,
    inside_buf: String,
}

impl IncrementalThinkStripper {
    pub fn new() -> Self {
        Self::default()
    }

    /// Consume the next delta and return text safe to append to a TTS buffer.
    pub fn push(&mut self, delta: &str) -> String {
        if self.inside {
            self.inside_buf.push_str(delta);
            self.drain_inside()
        } else {
            let combined = std::mem::take(&mut self.pending) + delta;
            self.drain_outside(combined)
        }
    }

    /// Mark end-of-stream; drop any partial opening tag or unclosed think block.
    pub fn flush(&mut self) -> String {
        self.inside_buf.clear();
        self.inside = false;
        let leftover = std::mem::take(&mut self.pending);
        if leftover.starts_with('<') {
            String::new()
        } else {
            leftover
        }
    }

    #[cfg(test)]
    pub fn is_inside(&self) -> bool {
        self.inside
    }

    fn drain_outside(&mut self, mut buf: String) -> String {
        let mut out = String::new();
        loop {
            match buf.find("<think") {
                Some(pos) => {
                    out.push_str(&buf[..pos]);
                    let rest = &buf[pos..];
                    if let Some(gt) = rest.find('>') {
                        self.inside = true;
                        self.inside_buf = rest[gt + 1..].to_string();
                        let drained = self.drain_inside();
                        out.push_str(&drained);
                        if !self.inside {
                            buf = std::mem::take(&mut self.pending);
                            continue;
                        }
                        break;
                    } else {
                        self.pending = rest.to_string();
                        break;
                    }
                }
                None => {
                    let safe_emit_end = tail_safe_emit_boundary(&buf);
                    out.push_str(&buf[..safe_emit_end]);
                    self.pending = buf[safe_emit_end..].to_string();
                    break;
                }
            }
        }
        out
    }

    fn drain_inside(&mut self) -> String {
        if let Some((pos, tag)) = find_close_tag(&self.inside_buf) {
            let after = self.inside_buf[pos + tag.len()..].to_string();
            self.inside_buf.clear();
            self.inside = false;
            self.pending = after;
            let buf = std::mem::take(&mut self.pending);
            return self.drain_outside(buf);
        }
        const TRAILING: usize = "</think".len();
        if self.inside_buf.len() > TRAILING {
            let cut = self.inside_buf.len() - TRAILING;
            let safe_cut = (0..=cut)
                .rev()
                .find(|&i| self.inside_buf.is_char_boundary(i))
                .unwrap_or(0);
            self.inside_buf.drain(..safe_cut);
        }
        String::new()
    }
}

fn tail_safe_emit_boundary(buf: &str) -> usize {
    const OPEN: &str = "<think";
    let max = OPEN.len() - 1;
    for k in (1..=max).rev() {
        if buf.len() < k {
            continue;
        }
        let start = buf.len() - k;
        if !buf.is_char_boundary(start) {
            continue;
        }
        let suffix = &buf[start..];
        if OPEN.starts_with(suffix) {
            return start;
        }
    }
    buf.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_complete_think_block() {
        let mut s = IncrementalThinkStripper::new();
        let out = s.push("before <think>secret</think> after");
        assert_eq!(out, "before  after");
    }

    #[test]
    fn drops_block_with_attributes() {
        let mut s = IncrementalThinkStripper::new();
        let out = s.push("x <think zh,>y</think>z");
        assert_eq!(out, "x z");
    }

    #[test]
    fn drops_unclosed_block_on_flush() {
        let mut s = IncrementalThinkStripper::new();
        assert_eq!(s.push("safe <think>still thinking"), "safe ");
        assert_eq!(s.flush(), "");
        assert!(!s.is_inside());
    }

    #[test]
    fn handles_thinking_close_tag() {
        let mut s = IncrementalThinkStripper::new();
        assert_eq!(s.push("pre <think>hidden</thi"), "pre ");
        assert_eq!(s.push("nk>post"), "post");
    }

    #[test]
    fn strip_think_blocks_removes_unterminated() {
        let input = "<think>secret</think>\n你好\n<thinking>tail";
        let out = strip_think_blocks(input);
        assert!(out.contains("你好"));
        assert!(!out.contains("secret"));
        assert!(!out.contains("tail"));
    }

    #[test]
    fn extract_inline_thinking_from_content_field() {
        let input = "<think>用户想查天气</think>\n明天可能下雨。";
        let thinking = extract_inline_thinking(input);
        assert!(thinking.contains("用户想查天气"));
        let speakable = strip_think_blocks(input);
        assert!(speakable.contains("明天可能下雨"));
        assert!(!speakable.contains("用户想查天气"));
    }

    #[test]
    fn board_log_unclosed_redacted_thinking_only() {
        let input = "<think>\n用户想知道明天的天气。\n我需要获取明天的天气信息。\n\
            但是我没有直接获取天气的工具，我需要调用 hermes 来帮我查询。\n\
            用户语气亲切，要求回答纯口语化，符合人设“小白”。\n\
            首先确认当前时间，以便准确描述“明天”。\n\
            然后用 hermes 查询天气。\n\
            最后根据查询结果，以口语化的方式回答用户。";
        let thinking = extract_inline_thinking(input);
        assert!(thinking.contains("用户想知道明天的天气"));
        assert!(strip_think_blocks(input).trim().is_empty());
    }

    #[test]
    fn board_log_closed_redacted_then_reply() {
        let input = "<think>\n用户提到“试了一下，点了你的那个”\n</think>\n\n\
            你指的是哪个\nzh,你指的是哪个";
        let thinking = extract_inline_thinking(input);
        assert!(thinking.contains("试了一下"));
        let speakable = strip_think_blocks(input);
        assert!(speakable.contains("你指的是哪个"));
        assert!(!speakable.contains("redacted_thinking"));
        assert!(!speakable.contains("用户提到"));
    }
}
