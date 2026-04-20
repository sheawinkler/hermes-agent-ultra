//! Text sanitisation for TTS input.
//!
//! Two concerns live here:
//!
//! 1. [`strip_markdown_for_tts`] — apply the exact same regex set Python's
//!    `_strip_markdown_for_tts` uses, in the same order. Chat assistants emit
//!    markdown that sounds awful when read aloud (code fences, bullets, hr
//!    lines, URLs). This function strips them but keeps the readable body.
//!
//! 2. [`IncrementalThinkStripper`] — Hermes models occasionally emit
//!    `<think>...</think>` blocks inside the public response stream. Those
//!    must never reach the speaker. Because we receive streaming deltas
//!    rather than a single complete string, the stripper is stateful: it
//!    accepts chunks, emits everything outside any `<think>` span, and
//!    buffers partial tags across chunk boundaries.

use std::sync::OnceLock;

use regex::Regex;

// ---------------------------------------------------------------------------
// Regex registry (lazy, shared)
// ---------------------------------------------------------------------------

/// Wraps `OnceLock<Regex>` so callers look like static globals.
fn compile(pattern: &'static str, cell: &'static OnceLock<Regex>) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("valid regex"))
}

fn md_code_block() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // (?s) = DOTALL, so `.` matches newlines like Python's `[\s\S]*?`.
    compile(r"(?s)```.*?```", &RE)
}
fn md_link() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    compile(r"\[([^\]]+)\]\([^)]+\)", &RE)
}
fn md_url() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    compile(r"https?://\S+", &RE)
}
fn md_bold() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    compile(r"\*\*(.+?)\*\*", &RE)
}
fn md_italic() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    compile(r"\*(.+?)\*", &RE)
}
fn md_inline_code() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    compile(r"`(.+?)`", &RE)
}
fn md_header() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    compile(r"(?m)^#+\s*", &RE)
}
fn md_list_item() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    compile(r"(?m)^\s*[-*]\s+", &RE)
}
fn md_hr() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    compile(r"---+", &RE)
}
fn md_excess_nl() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    compile(r"\n{3,}", &RE)
}

// ---------------------------------------------------------------------------
// Markdown strip
// ---------------------------------------------------------------------------

/// Remove markdown formatting that shouldn't be spoken aloud.
///
/// Order matches Python exactly — swapping the order would change output for
/// inputs that mix code fences with bold or links.
pub fn strip_markdown_for_tts(text: &str) -> String {
    let mut s = md_code_block().replace_all(text, " ").to_string();
    s = md_link().replace_all(&s, "$1").to_string();
    s = md_url().replace_all(&s, "").to_string();
    s = md_bold().replace_all(&s, "$1").to_string();
    s = md_italic().replace_all(&s, "$1").to_string();
    s = md_inline_code().replace_all(&s, "$1").to_string();
    s = md_header().replace_all(&s, "").to_string();
    s = md_list_item().replace_all(&s, "").to_string();
    s = md_hr().replace_all(&s, "").to_string();
    s = md_excess_nl().replace_all(&s, "\n\n").to_string();
    s.trim().to_string()
}

// ---------------------------------------------------------------------------
// Incremental think-block stripper
// ---------------------------------------------------------------------------

/// Stateful filter that removes `<think>...</think>` blocks from a streaming
/// text source.
///
/// States:
///
/// * **Outside** — emit text until we see `<think` (opening tag prefix).
///   Because `<think>` can span two deltas we also buffer any trailing `<`
///   that *might* be the start of `<think` and re-check on the next chunk.
/// * **Inside**  — silently drop everything until `</think>` is seen, then
///   return to Outside.
///
/// Per Python's behaviour, an unclosed `<think` tag at stream end is treated
/// as hidden content and dropped.
#[derive(Debug, Default)]
pub struct IncrementalThinkStripper {
    /// Holds characters that may be the prefix of an opening/closing tag. In
    /// Outside state we keep at most `len("<think")-1 = 5` chars. In Inside
    /// state the buffer is unused (we just scan for `</think>`).
    pending: String,
    inside: bool,
    /// During Inside mode we concatenate deltas here so we can search across
    /// chunk boundaries for `</think>`.
    inside_buf: String,
}

impl IncrementalThinkStripper {
    pub fn new() -> Self {
        Self::default()
    }

    /// Consume the next delta and return the text that should be emitted.
    ///
    /// The returned string is always safe to append to downstream buffers —
    /// any partial `<think...` tag is held back until the next call (or
    /// [`flush`](Self::flush)).
    pub fn push(&mut self, delta: &str) -> String {
        if self.inside {
            self.inside_buf.push_str(delta);
            self.drain_inside()
        } else {
            let combined = std::mem::take(&mut self.pending) + delta;
            self.drain_outside(combined)
        }
    }

    /// Mark end-of-stream. Any partial tag (Outside) or unclosed think block
    /// (Inside) is discarded, matching Python's behaviour of stripping
    /// orphan `<think` at the end of `sentence_buf` via the `_think_block_re`
    /// substitution in the end-of-text branch.
    pub fn flush(&mut self) -> String {
        self.inside_buf.clear();
        self.inside = false;
        let leftover = std::mem::take(&mut self.pending);
        // If the leftover does not even start an opening tag we can safely
        // emit it; otherwise drop per Python semantics.
        if leftover.starts_with('<') {
            String::new()
        } else {
            leftover
        }
    }

    /// True iff we are currently inside an unclosed `<think>` block. Useful
    /// for tests and diagnostics.
    pub fn is_inside(&self) -> bool {
        self.inside
    }

    // -- internal helpers -------------------------------------------------

    /// Handle the Outside state. `buf` is the pending remainder plus the
    /// new delta concatenated.
    fn drain_outside(&mut self, mut buf: String) -> String {
        let mut out = String::new();
        loop {
            match buf.find("<think") {
                Some(pos) => {
                    out.push_str(&buf[..pos]);
                    // Everything after `<think` is either the attr-list of the
                    // opening tag or a closing `>` + body. We need to find the
                    // closing `>` of the opening tag first. If it's not in
                    // this chunk we buffer and wait.
                    let rest = &buf[pos..];
                    if let Some(gt) = rest.find('>') {
                        // Successfully saw `<think...>`, switch to Inside.
                        self.inside = true;
                        // Consume the opening tag entirely.
                        self.inside_buf = rest[gt + 1..].to_string();
                        let drained = self.drain_inside();
                        out.push_str(&drained);
                        // Continue in whatever state drain_inside left us in.
                        if !self.inside {
                            // drain_inside emitted closing tag's trailing
                            // remainder back into `self.pending` via the
                            // `buf` return; re-enter Outside loop with that.
                            buf = std::mem::take(&mut self.pending);
                            continue;
                        }
                        break;
                    } else {
                        // Partial opening tag; buffer the whole `<think...`
                        // (no `>` yet) for the next delta.
                        self.pending = rest.to_string();
                        break;
                    }
                }
                None => {
                    // No `<think` prefix; but we may still have a trailing
                    // `<` that could start `<think` on the next delta.
                    let safe_emit_end = tail_safe_emit_boundary(&buf);
                    out.push_str(&buf[..safe_emit_end]);
                    self.pending = buf[safe_emit_end..].to_string();
                    break;
                }
            }
        }
        out
    }

    /// Handle the Inside state. Returns whatever text appears *after* a
    /// closing `</think>` tag if one is found in `self.inside_buf`.
    fn drain_inside(&mut self) -> String {
        match self.inside_buf.find("</think>") {
            Some(pos) => {
                let after = self.inside_buf[pos + "</think>".len()..].to_string();
                self.inside_buf.clear();
                self.inside = false;
                // `after` needs to go through Outside processing (it may
                // itself contain another `<think>` block, or a trailing
                // partial `<`).
                self.pending = after;
                let buf = std::mem::take(&mut self.pending);
                self.drain_outside(buf)
            }
            None => {
                // Closing tag not yet in buffer; could be split across
                // deltas. Keep at most 7 trailing chars (`</think` minus `>`)
                // in the buffer; the earlier body is safe to discard since
                // we're inside the block anyway.
                const TRAILING: usize = "</think".len();
                if self.inside_buf.len() > TRAILING {
                    let cut = self.inside_buf.len() - TRAILING;
                    // Find the nearest char boundary at or below `cut` to
                    // stay UTF-8 safe.
                    let safe_cut = (0..=cut)
                        .rev()
                        .find(|&i| self.inside_buf.is_char_boundary(i))
                        .unwrap_or(0);
                    self.inside_buf.drain(..safe_cut);
                }
                String::new()
            }
        }
    }
}

/// Compute the largest prefix length of `buf` that is *guaranteed* not to be
/// the start of a `<think` tag spanning into the next delta.
///
/// If `buf` ends with `<`, `<t`, `<th`, ..., `<thin` (anything that could be
/// the opening tag's prefix) we must hold it back.
fn tail_safe_emit_boundary(buf: &str) -> usize {
    const OPEN: &str = "<think";
    // We inspect up to len(OPEN)-1 = 5 trailing chars.
    let max = OPEN.len() - 1;
    // Check each suffix length from max down to 1.
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- strip_markdown_for_tts -----------------------------------------

    #[test]
    fn strip_markdown_keeps_plain_text() {
        assert_eq!(strip_markdown_for_tts("Hello world."), "Hello world.");
    }

    #[test]
    fn strip_markdown_removes_code_fence() {
        let input = "Pre ```rust\nlet x = 1;\n``` post";
        let out = strip_markdown_for_tts(input);
        assert!(!out.contains("```"));
        assert!(!out.contains("let x"));
        assert!(out.contains("Pre"));
        assert!(out.contains("post"));
    }

    #[test]
    fn strip_markdown_unwraps_link_and_drops_url() {
        let out = strip_markdown_for_tts(
            "See [the docs](https://example.com/guide) and https://raw.example.com/x.",
        );
        assert!(out.contains("See the docs"));
        assert!(!out.contains("https://"));
    }

    #[test]
    fn strip_markdown_inline_styles() {
        assert_eq!(
            strip_markdown_for_tts("This is **bold** and *italic* and `code`."),
            "This is bold and italic and code."
        );
    }

    #[test]
    fn strip_markdown_headers_lists_hr_and_extra_newlines() {
        let input = "# Title\n\n## Sub\n\n- item 1\n* item 2\n\n---\n\n\n\nEnd";
        let out = strip_markdown_for_tts(input);
        assert!(out.starts_with("Title"));
        assert!(out.contains("item 1"));
        assert!(out.contains("item 2"));
        assert!(!out.contains("---"));
        assert!(!out.contains("\n\n\n"));
    }

    // --- IncrementalThinkStripper ---------------------------------------

    #[test]
    fn think_stripper_passes_plain_text() {
        let mut s = IncrementalThinkStripper::new();
        assert_eq!(s.push("hello world"), "hello world");
        assert_eq!(s.flush(), "");
    }

    #[test]
    fn think_stripper_drops_complete_block() {
        let mut s = IncrementalThinkStripper::new();
        let out = s.push("before <think>secret</think> after");
        assert_eq!(out, "before  after");
        assert!(!s.is_inside());
    }

    #[test]
    fn think_stripper_handles_split_opening_tag() {
        let mut s = IncrementalThinkStripper::new();
        // Split `<think>` across three chunks.
        assert_eq!(s.push("head <th"), "head ");
        assert_eq!(s.push("ink>body"), "");
        assert_eq!(s.push("still</thi"), "");
        assert_eq!(s.push("nk>tail"), "tail");
        assert!(!s.is_inside());
    }

    #[test]
    fn think_stripper_handles_split_closing_tag() {
        let mut s = IncrementalThinkStripper::new();
        assert_eq!(s.push("pre <think>hidden</thi"), "pre ");
        assert_eq!(s.push("nk>post"), "post");
    }

    #[test]
    fn think_stripper_handles_attrs_on_opening_tag() {
        let mut s = IncrementalThinkStripper::new();
        let out = s.push("x <think class='foo'>y</think>z");
        assert_eq!(out, "x z");
    }

    #[test]
    fn think_stripper_drops_unclosed_block_on_flush() {
        let mut s = IncrementalThinkStripper::new();
        assert_eq!(s.push("safe <think>still thinking"), "safe ");
        assert_eq!(s.flush(), "");
        assert!(!s.is_inside());
    }

    #[test]
    fn think_stripper_drops_partial_tag_on_flush() {
        let mut s = IncrementalThinkStripper::new();
        assert_eq!(s.push("hi <th"), "hi ");
        // Flush while `<th` is still pending — per Python semantics we drop it.
        assert_eq!(s.flush(), "");
    }

    #[test]
    fn think_stripper_emits_lone_less_than() {
        let mut s = IncrementalThinkStripper::new();
        // `<abc` is never the prefix of `<think`, so emit immediately.
        assert_eq!(s.push("<abc>"), "<abc>");
    }

    #[test]
    fn think_stripper_two_blocks_in_one_chunk() {
        let mut s = IncrementalThinkStripper::new();
        let out = s.push("a<think>x</think>b<think>y</think>c");
        assert_eq!(out, "abc");
    }

    #[test]
    fn think_stripper_survives_unicode_in_body() {
        let mut s = IncrementalThinkStripper::new();
        let out = s.push("头 <think>私密思考…</think> 尾");
        assert_eq!(out, "头  尾");
    }

    #[test]
    fn tail_safe_boundary_holds_back_partial_prefix() {
        assert_eq!(tail_safe_emit_boundary("abc<th"), 3);
        assert_eq!(tail_safe_emit_boundary("abc<"), 3);
        assert_eq!(tail_safe_emit_boundary("abc<thin"), 3);
        assert_eq!(tail_safe_emit_boundary("abc"), 3);
        // `<apple` should not be held back — not a `<think` prefix.
        assert_eq!(tail_safe_emit_boundary("abc<apple"), "abc<apple".len());
    }
}
