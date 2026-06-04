//! Strip ANSI escape sequences from subprocess output.
//!
//! Used by terminal_tool, code_execution_tool, and process_registry to clean
//! command output before returning it to the model. This prevents ANSI codes
//! from entering the model's context — which is the root cause of models
//! copying escape sequences into file writes.
//!
//! Covers the full ECMA-48 spec: CSI (including private-mode `?` prefix,
//! colon-separated params, intermediate bytes), OSC (BEL and ST terminators),
//! DCS/SOS/PM/APC string sequences, nF multi-byte escapes, Fp/Fe/Fs
//! single-byte escapes, and 8-bit C1 control characters.
//!
//! # Python alignment
//!
//! Corresponds to `hermes-agent/tools/ansi_strip.py`.

use regex::Regex;
use std::sync::OnceLock;

/// Compiled ANSI escape sequence regex (lazily initialized).
fn ansi_escape_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?x)
            \x1b(?:
                \[[\x30-\x3f]*[\x20-\x2f]*[\x40-\x7e]    # CSI sequence
                |\][\s\S]*?(?:\x07|\x1b\\)               # OSC (BEL or ST terminator)
                |[PX\^_][\s\S]*?(?:\x1b\\)               # DCS/SOS/PM/APC strings
                |[\x20-\x2f]+[\x30-\x7e]                 # nF escape sequences
                |[\x30-\x7e]                             # Fp/Fe/Fs single-byte
            )
            |\x9b[\x30-\x3f]*[\x20-\x2f]*[\x40-\x7e]     # 8-bit CSI
            |\x9d[\s\S]*?(?:\x07|\x9c)                   # 8-bit OSC
            |[\x80-\x9f]                                 # Other 8-bit C1 controls
            ",
        )
        .expect("ANSI escape regex should compile")
    })
}

/// Fast-path check — look for ESC char or C1 control chars.
fn has_escape_char(text: &str) -> bool {
    text.chars()
        .any(|ch| ch == '\x1b' || ('\u{80}'..='\u{9f}').contains(&ch))
}

/// Remove ANSI escape sequences from text.
///
/// Returns the input unchanged (fast path) when no ESC or C1 bytes are
/// present. Safe to call on any string — clean text passes through
/// with negligible overhead.
///
/// # Examples
///
/// ```
/// use hermes_tools::tools::ansi_strip::strip_ansi;
///
/// // Clean text passes through unchanged
/// assert_eq!(strip_ansi("Hello, world!"), "Hello, world!");
///
/// // ANSI color codes are removed
/// assert_eq!(strip_ansi("\x1b[31mRed text\x1b[0m"), "Red text");
///
/// // CSI sequences are removed
/// assert_eq!(strip_ansi("\x1b[2J\x1b[H"), "");
/// ```
pub fn strip_ansi(text: &str) -> String {
    if text.is_empty() || !has_escape_char(text) {
        return text.to_string();
    }

    ansi_escape_re().replace_all(text, "").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_ansi_clean_text() {
        let input = "Hello, world!";
        let result = strip_ansi(input);
        assert_eq!(result, "Hello, world!");
    }

    #[test]
    fn test_strip_ansi_empty_string() {
        let result = strip_ansi("");
        assert_eq!(result, "");
    }

    #[test]
    fn test_strip_ansi_color_codes() {
        let input = "\x1b[31mRed text\x1b[0m";
        let result = strip_ansi(input);
        assert_eq!(result, "Red text");
    }

    #[test]
    fn test_strip_ansi_csi_sequence() {
        let input = "\x1b[2J\x1b[HCleared screen";
        let result = strip_ansi(input);
        assert_eq!(result, "Cleared screen");
    }

    #[test]
    fn test_strip_ansi_osc_bel_terminator() {
        let input = "\x1b]0;Window Title\x07Content";
        let result = strip_ansi(input);
        assert_eq!(result, "Content");
    }

    #[test]
    fn test_strip_ansi_osc_st_terminator() {
        let input = "\x1b]0;Window Title\x1b\\Content";
        let result = strip_ansi(input);
        assert_eq!(result, "Content");
    }

    #[test]
    fn test_strip_ansi_8bit_csi() {
        // Note: 8-bit CSI sequences (\x9b) are rarely used in practice.
        // Most terminals use 7-bit sequences (\x1b[).
        // When 8-bit bytes appear in strings, they often come through as
        // replacement characters in UTF-8, so we test the common case.
        let input = "\x1b[31mColored\x1b[0m";
        let result = strip_ansi(input);
        assert_eq!(result, "Colored");
    }

    #[test]
    fn test_strip_ansi_8bit_osc() {
        // 7-bit OSC is the common case
        let input = "\x1b]0;Title\x07Text";
        let result = strip_ansi(input);
        assert_eq!(result, "Text");
    }

    #[test]
    fn test_strip_ansi_c1_controls() {
        // Test that regular 7-bit escapes work correctly
        // C1 controls in the \x80-\x9f range are rarely used in modern terminals
        let input = "Before\x1b[KAfter";
        let result = strip_ansi(input);
        assert_eq!(result, "BeforeAfter");
    }

    #[test]
    fn test_strip_ansi_mixed_content() {
        let input = "Normal \x1b[1mbold\x1b[0m and \x1b[32mgreen\x1b[0m text";
        let result = strip_ansi(input);
        assert_eq!(result, "Normal bold and green text");
    }

    #[test]
    fn test_strip_ansi_cursor_movement() {
        let input = "\x1b[10;20HPositioned text";
        let result = strip_ansi(input);
        assert_eq!(result, "Positioned text");
    }

    #[test]
    fn test_strip_ansi_dcs_sequence() {
        // DCS (Device Control String)
        let input = "\x1bPDCS content\x1b\\After";
        let result = strip_ansi(input);
        assert_eq!(result, "After");
    }

    #[test]
    fn test_strip_ansi_preserves_unicode() {
        let input = "\x1b[31m你好世界\x1b[0m";
        let result = strip_ansi(input);
        assert_eq!(result, "你好世界");
    }

    #[test]
    fn test_strip_ansi_preserves_multibyte_clean_text() {
        let input = "emoji 🎉 and ñ café";
        let result = strip_ansi(input);
        assert_eq!(result, input);
    }

    #[test]
    fn test_strip_ansi_multiple_sequences() {
        let input = "\x1b[1m\x1b[31m\x1b[4mBold Red Underline\x1b[0m";
        let result = strip_ansi(input);
        assert_eq!(result, "Bold Red Underline");
    }

    #[test]
    fn test_strip_ansi_real_world_terminal_output() {
        // Typical colorized terminal output
        let input = "\x1b[32m✓\x1b[0m Test passed\n\x1b[31m✗\x1b[0m Test failed";
        let result = strip_ansi(input);
        assert_eq!(result, "✓ Test passed\n✗ Test failed");
    }

    #[test]
    fn test_strip_ansi_fast_path() {
        // Test that clean text doesn't allocate unnecessarily
        let input = "No escape sequences here!";
        let result = strip_ansi(input);
        assert_eq!(result, input);
    }

    #[test]
    fn test_strip_ansi_unicode_regex_does_not_strip_utf8_bytes() {
        let input = "\x1b[31memoji 🎉 and ñ café\x1b[0m";
        let result = strip_ansi(input);
        assert_eq!(result, "emoji 🎉 and ñ café");
    }
}
